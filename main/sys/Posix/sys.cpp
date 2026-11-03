// POSIX（Linux 等 Unix 系）平台实现。对应 main/include/ling/sys.hpp 的接口声明。
//
// 这一份只在非 Windows 上编译（CMake 里 else 分支把 main/sys/Posix 加进源列表）。
// 注意：从 S0 起只在 Windows 上编过，这份代码第一次上 Linux 要重新编译并跑回归。
#include "ling/sys.hpp"

#include <cerrno>
#include <chrono>
#include <csignal>
#include <cstdlib>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/wait.h>

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

std::filesystem::path default_data_root() {
    const auto exe_dir = executable_dir();
    if (!exe_dir.empty() && std::filesystem::exists(exe_dir / "ling.portable")) {
        return exe_dir / "data";
    }
    const std::string home = read_env("HOME");
    if (!home.empty()) {
        return std::filesystem::path(home) / ".local" / "share" / "ling";
    }
    return std::filesystem::path(".") / "data";
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
    // 不用 system("xdg-open '...'")：URL 里的单引号能逃逸成命令注入。
    // fork + execlp 不经过 shell，参数原样传给 xdg-open。
    const pid_t pid = ::fork();
    if (pid < 0) {
        if (error != nullptr) {
            *error = "fork 失败";
        }
        return false;
    }
    if (pid == 0) {
        // 子进程：丢掉输出，直接 exec。execlp 失败就退 127。
        const int devnull = ::open("/dev/null", O_RDWR);
        if (devnull >= 0) {
            ::dup2(devnull, STDOUT_FILENO);
            ::dup2(devnull, STDERR_FILENO);
            ::close(devnull);
        }
        ::execlp("xdg-open", "xdg-open", url.c_str(), static_cast<char*>(nullptr));
        ::_exit(127);
    }
    // 不阻塞等待：打开浏览器是异步动作，ling open 命令马上要退出。
    // 能回收就回收，不能回收的（xdg-open 还没退）等 ling open 退出后由 init 收养。
    ::waitpid(pid, nullptr, WNOHANG);
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
    // EINTR（被信号打断，如 Ctrl+C）时重试，直到超时或非 EINTR 错误；
    // 调用方只看到「就绪/超时/真错误」，不必自己处理信号打断。
    // select 失败时 fd_set 内容未指定（Linux 被信号打断可能已把它清空），
    // 所以重试前必须从副本还原，否则会把「有连接就绪」误判成「超时」。
    const fd_set original = *readfds;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    while (true) {
        const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now());
        timeval timeout{};
        timeout.tv_sec = static_cast<long>(left.count() / 1000);
        timeout.tv_usec = static_cast<long>((left.count() % 1000) * 1000);
        *readfds = original;  // 还原，见上：被打断后 fd_set 不可信
        const int ready = ::select(max_fd + 1, readfds, nullptr, nullptr, &timeout);
        if (ready >= 0 || errno != EINTR) {
            return ready;
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            return 0;  // 被信号一路打断到超时
        }
    }
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

std::tm utc_time(const std::time_t* seconds) {
    std::tm utc{};
    gmtime_r(seconds, &utc);
    return utc;
}

std::FILE* open_append(const std::filesystem::path& path) {
    return std::fopen(path.string().c_str(), "a");
}

bool atomic_replace(const std::filesystem::path& src, const std::filesystem::path& dst) {
    std::error_code ec;
    std::filesystem::rename(src, dst, ec);
    return !ec;
}

std::uint64_t file_stamp(const std::filesystem::path& path) {
    struct stat info {};
    if (::stat(path.c_str(), &info) != 0) {
        return 0;
    }
    const std::uint64_t time = static_cast<std::uint64_t>(info.st_mtim.tv_sec) * 1000000000ull +
                               static_cast<std::uint64_t>(info.st_mtim.tv_nsec);
    const std::uint64_t size = static_cast<std::uint64_t>(info.st_size);
    std::uint64_t seed = 1469598103934665603ull;  // FNV-1a 的起点
    for (const std::uint64_t part : {time, size}) {
        for (int i = 0; i < 8; ++i) {
            seed ^= (part >> (i * 8)) & 0xFFu;
            seed *= 1099511628211ull;
        }
    }
    return seed;
}

const char* shared_library_suffix() { return ".so"; }

}  // namespace sys
}  // namespace ling
