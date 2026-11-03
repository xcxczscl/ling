// core-ui 两个形态共用的「起服务」逻辑：握手、自检、绑定端口、拿到界面地址。
//
// webui 版与 desktop 版的区别只在「页面由谁显示」——浏览器还是 WebView2 窗口。
// 服务端、握手、口令这些是同一套，抽在这里，两个入口各自只写「怎么显示」。
#pragma once

#include <atomic>
#include <chrono>
#include <filesystem>
#include <memory>
#include <string>

#include "ui_server.hpp"

namespace ling {

struct UiApp {
    std::unique_ptr<ui::UiServer> server;
    std::filesystem::path data_root;
    std::string token;
    std::string url;          // http://127.0.0.1:<端口>/?token=... ，给浏览器或窗口导航用
    std::uint16_t port = 0;
    bool sidecar = false;     // 是宿主拉起的侧车，还是用户手动跑的
    std::atomic<bool> stop{false};        // 收到退出信号（Ctrl+C / deactivate / shutdown）
    std::atomic<bool> host_down{false};   // 宿主已经退出
    // 探测宿主的节拍，不每轮都开套接字。
    std::chrono::steady_clock::time_point last_probe{};
    int unreachable = 0;
};

// 完成从「读环境变量/参数」到「服务在监听」的全过程。返回 0 表示成功，
// 之后由调用方决定用什么方式显示页面；非 0 表示起不来，直接拿返回值退出。
// 起不来时已经把原因打到 stderr 了。
int ui_app_setup(int argc, char** argv, UiApp* app);

// 每轮要做的事：轮询服务、同步口令开关、探测宿主是否还活着。
// webui 与 desktop 的主循环都调它。返回 false 表示该收尾退出了。
bool ui_app_tick(UiApp* app);

// 收尾：停服务、清掉 ui_port。
void ui_app_cleanup(UiApp* app);

}  // namespace ling
