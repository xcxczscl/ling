// 平台抽象层统一接口。
//
// 宿主里所有「和操作系统打交道的动作」都收口到这里：取可执行文件目录、读环境变量、
// 命令行参数转码、控制台代码页、Ctrl+C、打开浏览器、动态库加载、套接字、本地时间、
// 打开文件、共享库后缀。业务代码（kernel、admin、ui_server、app、core-ui）只 include
// 本头文件，不再自己写 #ifdef _WIN32，也不再直接 include windows.h / sys/socket.h。
//
// 实现按平台拆成两份：main/Sys/Windows/sys.cpp 与 main/Sys/Posix/sys.cpp，构建时
// 由 CMake 按平台只编其中一份（见 main/CMakeLists.txt）。
//
// 为什么单独一层：同一个动作在两个平台的系统调用不一样（动态库是 LoadLibrary 对
// dlopen，套接字要 WSAStartup，控制台要切代码页），散在业务代码里就到处是 #ifdef，
// 平台一多（将来还有 Android）会越叠越乱。集中到这里，业务代码只剩「我要做什么」。
#pragma once

#include <cstdio>
#include <ctime>
#include <filesystem>
#include <string>

// 平台头文件统一在这里引入，业务代码不再自己写平台 include。
// 顺序有讲究：winsock2.h 必须在 windows.h 之前，否则会撞上 winsock.h 的重复定义。
#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <shellapi.h>
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
#include <dlfcn.h>
#endif

namespace ling {
namespace sys {

// ---- 路径与进程自身 ----

// 当前可执行文件所在目录；拿不到返回空路径。便携判定、找随包运行时都靠它。
std::filesystem::path executable_dir();

// 读环境变量，不存在返回空串。Windows 用 _dupenv_s（MSVC 把 getenv 标为不安全，
// 而项目 /WX 会把警告当错误），POSIX 用 getenv。
std::string read_env(const char* name);

// 命令行参数转码。Windows 把参数按系统代码页（中文系统是 936）交给程序，
// 内部一律 UTF-8，所以入口处要转一次；POSIX 的参数本来就是 UTF-8 字节，原样返回。
std::string from_ansi(const char* text);

// Windows 的宽字符（UTF-16）转换，给 Windows API（CreateProcessW 等）用；
// POSIX 没有 UTF-16 这回事，不提供这两个。
#ifdef _WIN32
std::string wide_to_utf8(const std::wstring& wide);
std::wstring utf8_to_wide(const std::string& text);
#endif

// ---- 控制台 ----

// RAII：构造时把控制台代码页切到 UTF-8，析构时还原（Windows）；POSIX 什么都不做。
// 不还原会污染同一个控制台里后续运行的其他程序。
class ConsoleUtf8Guard {
public:
    ConsoleUtf8Guard();
    ~ConsoleUtf8Guard();
    ConsoleUtf8Guard(const ConsoleUtf8Guard&) = delete;
    ConsoleUtf8Guard& operator=(const ConsoleUtf8Guard&) = delete;

private:
#ifdef _WIN32
    unsigned int previous_input_ = 0;
    unsigned int previous_output_ = 0;
#endif
};

// 注册 Ctrl+C 回调。handler 里只许置标志，真正的收尾在主循环做。
// Windows 用 SetConsoleCtrlHandler，POSIX 用 signal(SIGINT/SIGTERM)。
void register_ctrl_c(void (*handler)());

// ---- 外部命令 ----

// 用默认浏览器打开地址；失败返回 false，并把原因写进 error（给人看的话）。
bool open_in_browser(const std::string& url, std::string* error);

// ---- 动态库 ----

void* load_library(const std::filesystem::path& path);  // LoadLibraryA / dlopen
void* find_symbol(void* module, const char* name);      // GetProcAddress / dlsym
void unload_library(void* module);                      // FreeLibrary / dlclose

// ---- 套接字 ----

// 套接字句柄。Windows 是 SOCKET（无符号指针大小），POSIX 是 int。
// 业务代码用这个类型声明变量，直接调 ::socket / ::send / ::recv 等（两边同名）。
#ifdef _WIN32
using socket_handle = SOCKET;
#else
using socket_handle = int;
#endif

// 无效句柄。Windows 是 INVALID_SOCKET（~0），POSIX 是 -1。
inline socket_handle bad_socket() {
#ifdef _WIN32
    return INVALID_SOCKET;
#else
    return -1;
#endif
}

// 确保套接字库就绪。Windows 上必须 WSAStartup 才能用 Winsock；POSIX 不用，空函数。
void socket_startup();

// 关闭套接字：closesocket / close。
void close_socket(socket_handle fd);

// 上一次收发失败是否属于「超时 / 非阻塞下暂无数据」。这两种要告诉使用者「稍后重试」，
// 和「连接断了」的下一步完全不同。
bool socket_would_block();

// select 轮询可读。Windows 忽略第一个 nfds 参数（传 0 即可），POSIX 要传 max_fd+1，
// 这个差异收口在这里，业务代码只传「最大 fd」和超时。
int socket_select(socket_handle max_fd, fd_set* readfds, int timeout_ms);

// 设置套接字的收发超时（毫秒）。Windows 的 SO_RCVTIMEO/SO_SNDTIMEO 要 DWORD，
// POSIX 要 timeval，这个差异收口在这里。
void set_socket_timeout(socket_handle fd, int timeout_ms);

// ---- 时间与文件 ----

// 本地时间。MSVC 的 localtime_s 与 POSIX 的 localtime_r 签名不同，这里统一。
std::tm local_time(const std::time_t* seconds);

// 以追加方式打开文件。MSVC 的 fopen_s 与 POSIX 的 fopen 签名不同，这里统一。
std::FILE* open_append(const std::filesystem::path& path);

// 原子地把 src 替换为 dst（覆盖已存在的 dst）。Windows 的 rename 不覆盖已存在目标，
// 用 MoveFileEx 换「替换」语义；POSIX 的 rename 本身就是原子替换。
bool atomic_replace(const std::filesystem::path& src, const std::filesystem::path& dst);

// ---- 共享库后缀 ----

const char* shared_library_suffix();  // ".dll" / ".so"

}  // namespace sys
}  // namespace ling
