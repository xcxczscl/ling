#include "ling/sidecar_host.hpp"

#include <chrono>
#include <vector>
#include <thread>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <csignal>
#include <cstring>
#include <fcntl.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

namespace ling {
namespace {

#ifdef _WIN32
std::string to_utf8(const std::wstring& wide) {
    if (wide.empty()) {
        return {};
    }
    const int size = ::WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), -1, nullptr, 0, nullptr, nullptr);
    if (size <= 1) {
        return {};
    }
    std::string out(static_cast<std::size_t>(size), '\0');
    ::WideCharToMultiByte(CP_UTF8, 0, wide.c_str(), -1, out.data(), size, nullptr, nullptr);
    out.resize(static_cast<std::size_t>(size - 1));
    return out;
}

std::wstring to_wide(const std::string& text) {
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
#endif

}  // namespace

Sidecar::Sidecar() = default;

Sidecar::~Sidecar() { terminate(200); }

std::string Sidecar::start(const std::filesystem::path& executable,
                           const std::filesystem::path& working_dir, const std::string& plugin_id,
                           const std::string& token, const std::string& data_dir) {
#ifdef _WIN32
    SECURITY_ATTRIBUTES attributes{};
    attributes.nLength = sizeof(attributes);
    attributes.bInheritHandle = TRUE;

    HANDLE child_stdin_read = nullptr;
    HANDLE child_stdout_write = nullptr;
    HANDLE parent_stdin_write = nullptr;
    HANDLE parent_stdout_read = nullptr;

    if (!::CreatePipe(&child_stdin_read, &parent_stdin_write, &attributes, 0)) {
        return "无法创建输入管道";
    }
    if (!::SetHandleInformation(parent_stdin_write, HANDLE_FLAG_INHERIT, 0)) {
        return "无法设置输入管道";
    }
    if (!::CreatePipe(&parent_stdout_read, &child_stdout_write, &attributes, 0)) {
        return "无法创建输出管道";
    }
    if (!::SetHandleInformation(parent_stdout_read, HANDLE_FLAG_INHERIT, 0)) {
        return "无法设置输出管道";
    }

    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = child_stdin_read;
    startup.hStdOutput = child_stdout_write;
    startup.hStdError = child_stdout_write;  // 侧车的 stderr 与 stdout 合流，一起进帧流

    PROCESS_INFORMATION info{};
    std::wstring command = to_wide(executable.string());
    std::wstring cwd = to_wide(working_dir.string());

    // 环境变量：口令只走环境变量（§8.4），命令行参数在进程列表里全机可见。
    //
    // 自己拼环境块容易被一个格式细节拖进「错误码 87」这种没线索的坑里，
    // 改成「临时设三项 → 起进程 → 还原」。宿主是单线程内核，不存在并发读写环境的问题。
    const wchar_t* names[3] = {L"LING_TOKEN", L"LING_PLUGIN_ID", L"LING_DATA_DIR"};
    const std::wstring values[3] = {to_wide(token), to_wide(plugin_id), to_wide(data_dir)};
    std::wstring previous[3];
    bool had_previous[3] = {false, false, false};
    for (int i = 0; i < 3; ++i) {
        const DWORD size = ::GetEnvironmentVariableW(names[i], nullptr, 0);
        if (size > 0) {
            previous[i].resize(size);
            ::GetEnvironmentVariableW(names[i], previous[i].data(), size);
            previous[i].resize(size - 1);
            had_previous[i] = true;
        }
        ::SetEnvironmentVariableW(names[i], values[i].c_str());
    }

    const BOOL ok = ::CreateProcessW(nullptr, command.data(), nullptr, nullptr, TRUE,
                                     CREATE_NO_WINDOW, nullptr,
                                     cwd.empty() ? nullptr : cwd.c_str(), &startup, &info);

    // 起完立刻还原，免得这几个变量漏到后续起的别的进程里。
    for (int i = 0; i < 3; ++i) {
        ::SetEnvironmentVariableW(names[i], had_previous[i] ? previous[i].c_str() : nullptr);
    }
    ::CloseHandle(child_stdin_read);
    ::CloseHandle(child_stdout_write);
    if (!ok) {
        ::CloseHandle(parent_stdin_write);
        ::CloseHandle(parent_stdout_read);
        return "侧车进程起不来（" + executable.filename().string() + "，错误码 " +
               std::to_string(::GetLastError()) + "）";
    }

    ::CloseHandle(info.hThread);
    process_ = reinterpret_cast<std::intptr_t>(info.hProcess);
    stdin_write_ = reinterpret_cast<std::intptr_t>(parent_stdin_write);
    stdout_read_ = reinterpret_cast<std::intptr_t>(parent_stdout_read);
    pid_ = static_cast<long>(info.dwProcessId);
    return {};
#else
    (void)executable;
    (void)working_dir;
    (void)plugin_id;
    (void)token;
    (void)data_dir;
    // POSIX 侧车要到能编译 Linux 那一半时再做：这里先明确说「没做」，
    // 不假装支持（docs/log/047）。
    return "侧车轨道目前在 Windows 上实现，这个平台还没做";
#endif
}

bool Sidecar::send(const sidecar::Frame& frame, std::string* error) {
#ifdef _WIN32
    if (stdin_write_ == 0) {
        *error = "侧车已经不在运行";
        return false;
    }
    const std::string line = sidecar::encode(frame);
    DWORD written = 0;
    if (!::WriteFile(reinterpret_cast<HANDLE>(stdin_write_), line.data(),
                     static_cast<DWORD>(line.size()), &written, nullptr) ||
        written != line.size()) {
        *error = "写管道失败";
        return false;
    }
    return true;
#else
    (void)frame;
    *error = "侧车轨道目前在 Windows 上实现";
    return false;
#endif
}

bool Sidecar::read(sidecar::Frame* out, int timeout_ms, bool* closed, std::string* error) {
    *closed = false;
#ifdef _WIN32
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    while (true) {
        // 先看已经收到的字节里有没有一整行。
        const auto newline = pending_.find('\n');
        if (newline != std::string::npos) {
            const std::string line = pending_.substr(0, newline);
            pending_.erase(0, newline + 1);
            if (line.empty()) {
                continue;
            }
            return sidecar::parse(line, out, error);
        }

        if (stdout_read_ == 0) {
            *closed = true;
            return false;
        }
        DWORD available = 0;
        if (!::PeekNamedPipe(reinterpret_cast<HANDLE>(stdout_read_), nullptr, 0, nullptr,
                             &available, nullptr)) {
            *closed = true;
            return false;
        }
        if (available > 0) {
            char chunk[4096];
            DWORD got = 0;
            if (!::ReadFile(reinterpret_cast<HANDLE>(stdout_read_), chunk, sizeof(chunk), &got,
                            nullptr)) {
                *closed = true;
                return false;
            }
            pending_.append(chunk, got);
            continue;
        }
        if (::WaitForSingleObject(reinterpret_cast<HANDLE>(process_), 0) == WAIT_OBJECT_0) {
            *closed = true;
            return false;
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            error->clear();
            return false;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
#else
    (void)out;
    (void)timeout_ms;
    *closed = true;
    *error = "侧车轨道目前在 Windows 上实现";
    return false;
#endif
}

bool Sidecar::alive() {
#ifdef _WIN32
    if (process_ == 0) {
        return false;
    }
    return ::WaitForSingleObject(reinterpret_cast<HANDLE>(process_), 0) != WAIT_OBJECT_0;
#else
    return false;
#endif
}

int Sidecar::exit_code() {
#ifdef _WIN32
    if (process_ == 0 || exited_) {
        return exit_code_;
    }
    DWORD code = 0;
    if (::GetExitCodeProcess(reinterpret_cast<HANDLE>(process_), &code) &&
        code != STILL_ACTIVE) {
        exit_code_ = static_cast<int>(code);
        exited_ = true;
        return exit_code_;
    }
    return -1;
#else
    return -1;
#endif
}

void Sidecar::terminate(int wait_ms) {
#ifdef _WIN32
    if (process_ == 0) {
        return;
    }
    auto* handle = reinterpret_cast<HANDLE>(process_);
    if (::WaitForSingleObject(handle, static_cast<DWORD>(wait_ms)) != WAIT_OBJECT_0) {
        ::TerminateProcess(handle, 1);
        ::WaitForSingleObject(handle, 2000);
    }
    DWORD code = 0;
    if (::GetExitCodeProcess(handle, &code)) {
        exit_code_ = static_cast<int>(code);
    }
    exited_ = true;
    close_handles();
    ::CloseHandle(handle);
    process_ = 0;
#else
    (void)wait_ms;
#endif
}

void Sidecar::close_handles() {
#ifdef _WIN32
    if (stdin_write_ != 0) {
        ::CloseHandle(reinterpret_cast<HANDLE>(stdin_write_));
        stdin_write_ = 0;
    }
    if (stdout_read_ != 0) {
        ::CloseHandle(reinterpret_cast<HANDLE>(stdout_read_));
        stdout_read_ = 0;
    }
#endif
}

long Sidecar::pid() const { return pid_; }

}  // namespace ling
