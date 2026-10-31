// 界面进程的 HTTP 服务端：回环监听、口令认证、把 /api/<动作> 转成管理端口调用。
// 语义依据 docs/ling/builtin-plugins/core-ui/02-服务端与接口.md。
#pragma once

#include <cstdint>
#include <filesystem>
#include <memory>
#include <string>

namespace ling {

// 界面进程对外提供的那一层。它自己不碰内核，每个动作都转成一次管理端口调用，
// 所以界面与命令行的行为永远一致（同一条调用路由、同一套错误码）。
class UiServer {
public:
    struct Options {
        std::filesystem::path data_root;
        std::string token;                  // 与 session.json 里的口令相同
        std::uint16_t start_port = 8010;    // 占用则顺延，最多到 last_port
        std::uint16_t last_port = 8099;
    };

    UiServer();
    ~UiServer();

    UiServer(const UiServer&) = delete;
    UiServer& operator=(const UiServer&) = delete;

    // 绑端口并进入可服务状态。返回空表示成功，否则是给人看的一句原因。
    std::string start(const Options& options);

    std::uint16_t port() const;

    // 处理一轮事件，最多等 timeout_ms。
    void poll(int timeout_ms);

    void stop();

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace ling
