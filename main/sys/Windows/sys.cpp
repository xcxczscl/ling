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
    char* value = nullptr;
    std::size_t length = 0;
    if (_dupenv_s(&value, &length, name) != 0 || value == nullptr) {
        return {};
    }
    std::string result(value);
    std::free(value);
    return result;
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
    return reinterpret_cast<void*>(::LoadLibraryA(path.string().c_str()));
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

std::FILE* open_append(const std::filesystem::path& path) {
    std::FILE* file = nullptr;
    fopen_s(&file, path.string().c_str(), "a");
    return file;
}

bool atomic_replace(const std::filesystem::path& src, const std::filesystem::path& dst) {
    return ::MoveFileExA(src.string().c_str(), dst.string().c_str(),
                         MOVEFILE_REPLACE_EXISTING) != 0;
}

const char* shared_library_suffix() { return ".dll"; }

}  // namespace sys
}  // namespace ling
