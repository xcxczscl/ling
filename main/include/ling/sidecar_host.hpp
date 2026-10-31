// 侧车进程：起一个子进程，用两根管道跑 jsonl 帧。
// 协议见 docs/ling/04-详细设计/02-插件装载与接口.md §8；这里只管进程与管道。
#pragma once

#include <cstdint>
#include <filesystem>
#include <string>

#include "ling/sidecar_proto.hpp"

namespace ling {

class Sidecar {
public:
    Sidecar();
    ~Sidecar();

    Sidecar(const Sidecar&) = delete;
    Sidecar& operator=(const Sidecar&) = delete;

    // 起进程。环境变量里带 LING_TOKEN / LING_PLUGIN_ID / LING_DATA_DIR，
    // 口令只经环境变量传，命令行参数在进程列表里对所有用户可见（§8.4）。
    std::string start(const std::filesystem::path& executable,
                      const std::filesystem::path& working_dir, const std::string& plugin_id,
                      const std::string& token, const std::string& data_dir);

    bool send(const sidecar::Frame& frame, std::string* error);

    // 读一帧，最多等 timeout_ms。返回 false 时看 closed：
    // true = 进程已退出或断开；false = 只是这段时间没有帧。
    bool read(sidecar::Frame* out, int timeout_ms, bool* closed, std::string* error);

    bool alive();
    int exit_code();
    long pid() const;

    // 先礼后兵：等它自己退 wait_ms 毫秒，不退就杀。
    void terminate(int wait_ms);

private:
    void close_handles();

    std::intptr_t process_ = 0;
    std::intptr_t stdin_write_ = 0;
    std::intptr_t stdout_read_ = 0;
    long pid_ = 0;
    int exit_code_ = -1;
    bool exited_ = false;
    // 已读到但还没凑成整行的字节。一次 read 可能带回两帧，剩下的必须留着，
    // 否则第二帧会被吞掉——表现是「偶尔丢一条应答」。
    std::string pending_;
};

}  // namespace ling
