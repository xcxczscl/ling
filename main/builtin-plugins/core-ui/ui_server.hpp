// 界面进程的 HTTP 服务端。
//
// 语义依据 docs/ling/builtin-plugins/core-ui/04-详细设计/01-服务端与接口.md。
// 四类路由：外壳静态文件（免认证）、面板资源（免认证）、theme.css（免认证）、
// /api/*（bearer token）。有副作用的调用一律落在第四类。
#pragma once

#include <cstdint>
#include <filesystem>
#include <memory>
#include <string>

namespace ling {
namespace ui {

// 界面进程对外提供的那一层。它自己不碰内核，每个动作都转成一次管理端口调用，
// 所以界面与命令行的行为永远一致（同一条调用路由、同一套错误码）。
class UiServer {
public:
    struct Options {
        std::filesystem::path data_root;
        std::string token;                  // 与 session.json 里的口令相同
        // 关掉口令校验时，浏览器直接打开就能用，不用带 token。
        // 只绑 127.0.0.1，关掉等于「本机所有用户都能用」，单用户机器可选。
        bool require_auth = true;
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

    // 运行中切换要不要口令。用户改设置后由调用方（core-ui）读出来同步过来。
    void set_require_auth(bool value);

    // 处理一轮事件，最多等 timeout_ms。
    void poll(int timeout_ms);

    void stop();

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

// 界面进程自己的版本，写进「关于」页与 plugin.toml。
const char* ui_version();

}  // namespace ui
}  // namespace ling
