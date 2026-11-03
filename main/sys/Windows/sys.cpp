// Windows 平台实现。对应 main/include/ling/sys.hpp 的接口声明。
//
// 这一份只在 Windows 上编译（CMake 里 WIN32 分支才把 main/sys/Windows 加进源列表）。
// 每个函数头上不重复写「Windows 版」——整个文件都是 Windows 版。
#include "ling/sys.hpp"

#include <chrono>
#include <cstdlib>

namespace ling {
namespace sys {

// ---- 路径与进程自身 ----

std::filesystem::path executable_dir() {
    std::wstring buf(32768, L'\0');
    const DWORD n = GetModuleFileNameW(nullptr, buf.data(), static_cast<DWORD>(buf.size()));
    if (n == 0) {
        return {};
    }
    return std::filesystem::path(buf.substr(0, n)).parent_path();
}

std::string read_env(const char* name) {
#if defined(_MSC_VER)
    char* value = nullptr;
    std::size_t length = 0;
    if (_dupenv_s(&value, &length, name) != 0 || value == nullptr) {
        return {};
    }
    std::string result(value);
    std::free(value);
    return result;
#else
    // MinGW 没有 _dupenv_s。getenv 返回的是进程环境块的指针，只读一次即可，
    // 这里的调用都发生在启动阶段，不涉及并发。
    const char* value = std::getenv(name);
    return value == nullptr ? std::string() : std::string(value);
#endif
}

std::filesystem::path default_data_root() {
    const auto exe_dir = executable_dir();
    if (!exe_dir.empty() && std::filesystem::exists(exe_dir / "ling.portable")) {
        return exe_dir / "data";
    }
    const std::string appdata = read_env("APPDATA");
    if (!appdata.empty()) {
        return std::filesystem::path(appdata) / "ling";
    }
    return std::filesystem::path(".") / "data";
}

// UTF-8 与宽字符互转。Windows API 走 UTF-16，程序内部走 UTF-8，两套编码的桥。
std::string wide_to_utf8(const std::wstring& wide) {
    if (wide.empty()) {
        return {};
    }
    const int size =
        ::WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), -1, nullptr, 0, nullptr, nullptr);
    if (size <= 1) {
        return {};
    }
    std::string out(static_cast<std::size_t>(size), '\0');
    ::WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), -1, out.data(), size, nullptr, nullptr);
    out.resize(static_cast<std::size_t>(size - 1));
    return out;
}

std::wstring utf8_to_wide(const std::string& text) {
    if (text.empty()) {
        return {};
    }
    const int size = ::MultiByteToWideChar(CP_UTF8, 0, text.c_str(), -1, nullptr, 0);
    if (size <= 1) {
        return {};
    }
    std::wstring out(static_cast<std::size_t>(size), L'\0');
    ::MultiByteToWideChar(CP_UTF8, 0, text.c_str(), -1, out.data(), size);
    out.resize(static_cast<std::size_t>(size - 1));
    return out;
}

std::string from_ansi(const char* text) {
    if (text == nullptr || *text == '\0') {
        return {};
    }
    const int wide_len = ::MultiByteToWideChar(CP_ACP, 0, text, -1, nullptr, 0);
    if (wide_len <= 1) {
        return std::string(text);
    }
    std::wstring wide(static_cast<std::size_t>(wide_len), L'\0');
    ::MultiByteToWideChar(CP_ACP, 0, text, -1, wide.data(), wide_len);
    return wide_to_utf8(wide);
}

// ---- 控制台 ----

ConsoleUtf8Guard::ConsoleUtf8Guard() {
    previous_input_ = ::GetConsoleCP();
    previous_output_ = ::GetConsoleOutputCP();
    if (previous_output_ != CP_UTF8) {
        ::SetConsoleOutputCP(CP_UTF8);
    }
    if (previous_input_ != CP_UTF8) {
        ::SetConsoleCP(CP_UTF8);
    }
}

ConsoleUtf8Guard::~ConsoleUtf8Guard() {
    if (previous_output_ != 0 && previous_output_ != CP_UTF8) {
        ::SetConsoleOutputCP(previous_output_);
    }
    if (previous_input_ != 0 && previous_input_ != CP_UTF8) {
        ::SetConsoleCP(previous_input_);
    }
}

namespace {
// SetConsoleCtrlHandler 要 BOOL WINAPI(DWORD) 签名的回调，业务侧只想传「置标志」的
// 无参函数，这里桥一层。
void (*g_ctrl_c_handler)() = nullptr;

BOOL WINAPI ctrl_c_dispatch(DWORD type) {
    (void)type;
    if (g_ctrl_c_handler != nullptr) {
        g_ctrl_c_handler();
    }
    return TRUE;
}
}  // namespace

void register_ctrl_c(void (*handler)()) {
    g_ctrl_c_handler = handler;
    ::SetConsoleCtrlHandler(ctrl_c_dispatch, TRUE);
}

// ---- 外部命令 ----

bool open_in_browser(const std::string& url, std::string* error) {
    const auto result = reinterpret_cast<std::intptr_t>(
        ::ShellExecuteW(nullptr, L"open", utf8_to_wide(url).c_str(), nullptr, nullptr,
                        SW_SHOWNORMAL));
    if (result <= 32) {
        if (error != nullptr) {
            *error = "错误码 " + std::to_string(result);
        }
        return false;
    }
    return true;
}

// ---- 动态库 ----

void* load_library(const std::filesystem::path& path) {
    // 宽字符入口：路径里有中文时 LoadLibraryA 会按 ANSI 解释而找不到文件。
    return reinterpret_cast<void*>(::LoadLibraryW(path.wstring().c_str()));
}

void* find_symbol(void* module, const char* name) {
    return reinterpret_cast<void*>(::GetProcAddress(reinterpret_cast<HMODULE>(module), name));
}

void unload_library(void* module) {
    ::FreeLibrary(reinterpret_cast<HMODULE>(module));
}

// ---- 套接字 ----

void socket_startup() {
    static const bool ready = [] {
        WSADATA data{};
        return ::WSAStartup(MAKEWORD(2, 2), &data) == 0;
    }();
    (void)ready;
}

void close_socket(socket_handle fd) { ::closesocket(fd); }

bool socket_would_block() {
    const int code = ::WSAGetLastError();
    return code == WSAETIMEDOUT || code == WSAEWOULDBLOCK;
}

int socket_select(socket_handle max_fd, fd_set* readfds, int timeout_ms) {
    (void)max_fd;  // Windows 忽略 nfds
    // EINTR（被信号打断）时重试，直到超时或非 EINTR 错误；调用方只看到「就绪/超时/真错误」。
    // select 失败时 fd_set 内容未指定，重试前从副本还原，避免把「就绪」误判成「超时」。
    const fd_set original = *readfds;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    while (true) {
        const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now());
        timeval timeout{};
        timeout.tv_sec = static_cast<long>(left.count() / 1000);
        timeout.tv_usec = static_cast<long>((left.count() % 1000) * 1000);
        *readfds = original;  // 还原，见上：被打断后 fd_set 不可信
        const int ready = ::select(0, readfds, nullptr, nullptr, &timeout);
        if (ready != SOCKET_ERROR || ::WSAGetLastError() != WSAEINTR) {
            return ready;
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            return 0;  // 被信号一路打断到超时
        }
    }
}

void set_socket_timeout(socket_handle fd, int timeout_ms) {
    const DWORD t = static_cast<DWORD>(timeout_ms);
    ::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, reinterpret_cast<const char*>(&t), sizeof(t));
    ::setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, reinterpret_cast<const char*>(&t), sizeof(t));
}

// ---- 时间与文件 ----

std::tm local_time(const std::time_t* seconds) {
    std::tm local{};
    localtime_s(&local, seconds);
    return local;
}

std::tm utc_time(const std::time_t* seconds) {
    std::tm utc{};
    gmtime_s(&utc, seconds);
    return utc;
}

std::FILE* open_append(const std::filesystem::path& path) {
    // _wfopen_s 直接用宽字符路径，中文路径不会因为 ANSI 转换失败而打不开；
    // 用带 _s 的版本是为了通过 /WX 下的 C4996 检查。
    std::FILE* file = nullptr;
    _wfopen_s(&file, path.wstring().c_str(), L"a");
    return file;
}

bool atomic_replace(const std::filesystem::path& src, const std::filesystem::path& dst) {
    // MoveFileExA 只认 ANSI；换宽字符版本，中文文件名才替换得掉。
    return ::MoveFileExW(src.wstring().c_str(), dst.wstring().c_str(),
                         MOVEFILE_REPLACE_EXISTING) != 0;
}

std::uint64_t file_stamp(const std::filesystem::path& path) {
    WIN32_FILE_ATTRIBUTE_DATA info{};
    if (::GetFileAttributesExW(path.wstring().c_str(), GetFileExInfoStandard, &info) == 0) {
        return 0;
    }
    // FILETIME 是 1601 年起的 100 纳秒计数，高 32 位秒、低 32 位零头，拼回来才是原值。
    const std::uint64_t time = (static_cast<std::uint64_t>(info.ftLastWriteTime.dwHighDateTime) << 32) |
                               info.ftLastWriteTime.dwLowDateTime;
    const std::uint64_t size = (static_cast<std::uint64_t>(info.nFileSizeHigh) << 32) |
                               info.nFileSizeLow;
    // 两者都折进一个指纹：只有时间会漏掉「同一时刻改成长度不同的内容」，
    // 只有大小会漏掉「换成等长的内容」，合起来够用。
    std::uint64_t seed = 1469598103934665603ull;  // FNV-1a 的起点
    for (const std::uint64_t part : {time, size}) {
        for (int i = 0; i < 8; ++i) {
            seed ^= (part >> (i * 8)) & 0xFFu;
            seed *= 1099511628211ull;
        }
    }
    return seed;
}

const char* shared_library_suffix() { return ".dll"; }

}  // namespace sys
}  // namespace ling
