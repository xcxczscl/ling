// Sidecar 的 POSIX（Linux 等 Unix 系）实现。对应 main/include/ling/sidecar_host.hpp。
//
// 起子进程用 fork + exec（经 /bin/sh 展开命令），两根匿名管道跑 jsonl 帧，
// 读写用 write/read/poll，进程状态用 waitpid。口令经 setenv 传给子进程，
// 不拼进命令行（命令行参数在进程列表里全机可见，04/02 §8.4）。
//
// 注意：从 S0 起只在 Windows 上编过，这一份第一次上 Linux 要重新编译并跑 l4_sidecar。
#include "ling/sidecar_host.hpp"

#include "ling/sys.hpp"

#include <cerrno>
#include <csignal>
#include <chrono>
#include <poll.h>
#include <sys/wait.h>
#include <thread>

namespace ling {

Sidecar::Sidecar() = default;
Sidecar::~Sidecar() { terminate(200); }

std::string Sidecar::start(const std::filesystem::path& executable, const std::string& args,
                           const std::filesystem::path& working_dir, const std::string& plugin_id,
                           const std::string& token, const std::string& data_dir,
                           const std::string& sdk_dir, const UiEnv& ui) {
    // POSIX 的文件描述符用 -1 作「无效」哨兵（fd 0 是合法的，比如宿主关了 stdin 时
    // pipe 就会拿到 0）。Windows 用 nullptr（0）作哨兵，两边语义不同，各写各的。
    stdin_write_ = -1;
    stdout_read_ = -1;
    int in_pipe[2] = {-1, -1};
    int out_pipe[2] = {-1, -1};
    if (::pipe(in_pipe) != 0) {
        return "无法创建输入管道";
    }
    if (::pipe(out_pipe) != 0) {
        ::close(in_pipe[0]);
        ::close(in_pipe[1]);
        return "无法创建输出管道";
    }

    std::string command = executable.string();
    if (!args.empty()) {
        command += " ";
        command += args;
    }

    const pid_t pid = ::fork();
    if (pid < 0) {
        ::close(in_pipe[0]);
        ::close(in_pipe[1]);
        ::close(out_pipe[0]);
        ::close(out_pipe[1]);
        return "无法创建侧车进程";
    }
    if (pid == 0) {
        ::dup2(in_pipe[0], STDIN_FILENO);
        ::dup2(out_pipe[1], STDOUT_FILENO);
        ::dup2(out_pipe[1], STDERR_FILENO);
        ::close(in_pipe[0]);
        ::close(in_pipe[1]);
        ::close(out_pipe[0]);
        ::close(out_pipe[1]);
        ::setenv("LING_TOKEN", token.c_str(), 1);
        ::setenv("LING_PLUGIN_ID", plugin_id.c_str(), 1);
        ::setenv("LING_DATA_DIR", data_dir.c_str(), 1);
        ::setenv("LING_SDK_DIR", sdk_dir.c_str(), 1);
        // 只有 core-ui 拿得到这三项：口令是管理端口的，不是侧车握手口令。
        if (plugin_id == "core-ui") {
            if (!ui.token.empty()) {
                ::setenv("LING_UI_TOKEN", ui.token.c_str(), 1);
            }
            if (!ui.root.empty()) {
                ::setenv("LING_UI_ROOT", ui.root.c_str(), 1);
            }
            if (!ui.port.empty()) {
                ::setenv("LING_UI_PORT", ui.port.c_str(), 1);
            }
        }
        if (!working_dir.empty()) {
            ::chdir(working_dir.c_str());
        }
        ::execl("/bin/sh", "sh", "-c", command.c_str(), static_cast<char*>(nullptr));
        ::_exit(127);
    }

    ::close(in_pipe[0]);
    ::close(out_pipe[1]);
    process_ = static_cast<std::intptr_t>(pid);
    stdin_write_ = static_cast<std::intptr_t>(in_pipe[1]);
    stdout_read_ = static_cast<std::intptr_t>(out_pipe[0]);
    pid_ = static_cast<long>(pid);
    return {};
}

bool Sidecar::send(const sidecar::Frame& frame, std::string* error) {
    if (stdin_write_ < 0) {
        *error = "侧车已经不在运行";
        return false;
    }
    const std::string line = sidecar::encode(frame);
    std::size_t sent = 0;
    while (sent < line.size()) {
        const ssize_t n =
            ::write(static_cast<int>(stdin_write_), line.data() + sent, line.size() - sent);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            *error = "写管道失败";
            return false;
        }
        sent += static_cast<std::size_t>(n);
    }
    return true;
}

bool Sidecar::read(sidecar::Frame* out, int timeout_ms, bool* closed, std::string* error) {
    *closed = false;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    while (true) {
        const auto newline = pending_.find('\n');
        if (newline != std::string::npos) {
            const std::string line = pending_.substr(0, newline);
            pending_.erase(0, newline + 1);
            if (line.empty()) {
                continue;
            }
            return sidecar::parse(line, out, error);
        }
        if (stdout_read_ < 0) {
            *closed = true;
            return false;
        }
        const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now());
        pollfd item{};
        item.fd = static_cast<int>(stdout_read_);
        item.events = POLLIN;
        const int ready = ::poll(&item, 1, left.count() > 0 ? static_cast<int>(left.count()) : 0);
        if (ready > 0 && (item.revents & POLLIN) != 0) {
            char chunk[4096];
            const ssize_t got = ::read(static_cast<int>(stdout_read_), chunk, sizeof(chunk));
            if (got < 0) {
                if (errno == EINTR) {
                    continue;  // 被信号打断，重试；剩余超时由下一轮 poll 重新算
                }
                *closed = true;
                return false;
            }
            if (got == 0) {
                *closed = true;  // 对端关闭
                return false;
            }
            pending_.append(chunk, static_cast<std::size_t>(got));
            continue;
        }
        if (ready == 0) {
            error->clear();
            return false;
        }
        if (errno == EINTR) {
            continue;  // poll 被信号打断（如 Ctrl+C），重试而不是当成断开
        }
        *closed = true;
        return false;
    }
}

bool Sidecar::alive() {
    if (process_ == 0) {
        return false;
    }
    int status = 0;
    const pid_t done = ::waitpid(static_cast<pid_t>(process_), &status, WNOHANG);
    if (done == static_cast<pid_t>(process_)) {
        exited_ = true;
        exit_code_ = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        // waitpid 一旦返回 pid 就把子进程回收了，这个 pid 从此失效（可能被系统复用）。
        // 立即置 0，否则 terminate() 会对一个已回收的 pid 再 kill，可能误杀无关进程。
        process_ = 0;
        return false;
    }
    return done == 0;
}

int Sidecar::exit_code() {
    if (process_ == 0 || exited_) {
        return exit_code_;
    }
    int status = 0;
    const pid_t done = ::waitpid(static_cast<pid_t>(process_), &status, WNOHANG);
    if (done == static_cast<pid_t>(process_)) {
        exited_ = true;
        exit_code_ = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        process_ = 0;  // 同上：回收后 pid 失效，置 0
        return exit_code_;
    }
    return -1;
}

void Sidecar::terminate(int wait_ms) {
    if (process_ != 0) {
        const pid_t pid = static_cast<pid_t>(process_);
        int status = 0;
        if (::waitpid(pid, &status, WNOHANG) != pid) {
            ::kill(pid, SIGTERM);
            const auto deadline =
                std::chrono::steady_clock::now() + std::chrono::milliseconds(wait_ms);
            bool done = false;
            while (std::chrono::steady_clock::now() < deadline) {
                if (::waitpid(pid, &status, WNOHANG) == pid) {
                    done = true;
                    break;
                }
                std::this_thread::sleep_for(std::chrono::milliseconds(20));
            }
            if (!done) {
                ::kill(pid, SIGKILL);
                ::waitpid(pid, &status, 0);
            }
        }
        exit_code_ = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        exited_ = true;
        process_ = 0;
    }
    // 管道与进程生命周期分离：进程可能已被 alive()/exit_code() 回收（process_ 已置 0），
    // 但管道还开着，这里无论进程是否回收都要关。
    close_handles();
}

void Sidecar::close_handles() {
    if (stdin_write_ >= 0) {
        ::close(static_cast<int>(stdin_write_));
        stdin_write_ = -1;
    }
    if (stdout_read_ >= 0) {
        ::close(static_cast<int>(stdout_read_));
        stdout_read_ = -1;
    }
}

long Sidecar::pid() const { return pid_; }

}  // namespace ling
