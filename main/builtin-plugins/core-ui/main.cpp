// core-ui 的 webui 形态：只起服务，页面由浏览器打开（ling open 或手动）。
//
// 与 desktop 形态共用 app.cpp 里的「握手、自检、起服务」，这里的差别只有
// 不创建窗口、不进 Win32 消息循环。
#include <cstdio>

#include "app.h"
#include "ling/sys.hpp"

int main(int argc, char** argv) {
    ling::sys::ConsoleUtf8Guard console_utf8;
    (void)console_utf8;

    ling::UiApp app;
    const int setup = ling::ui_app_setup(argc, argv, &app);
    if (setup != 0) {
        return setup;
    }
    while (ling::ui_app_tick(&app)) {
    }
    ling::ui_app_cleanup(&app);
    return 0;
}
