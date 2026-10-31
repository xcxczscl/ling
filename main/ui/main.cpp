// core-ui 的 webui 形态：只起服务，页面由浏览器打开（ling open 或手动）。
//
// 与 desktop 形态共用 main/ui/app.cpp 里的「握手、自检、起服务」，这里的差别只有
// 不创建窗口、不进 Win32 消息循环。
#include <cstdio>

#include "app.h"

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace {

#ifdef _WIN32
// 控制台按 GBK 解读 UTF-8 会出乱码，退出时还原，避免污染同一个窗口。
class ConsoleUtf8Scope {
public:
    ConsoleUtf8Scope() {
        in_ = ::GetConsoleCP();
        out_ = ::GetConsoleOutputCP();
        ::SetConsoleCP(CP_UTF8);
        ::SetConsoleOutputCP(CP_UTF8);
    }
    ~ConsoleUtf8Scope() {
        if (in_ != 0) {
            ::SetConsoleCP(in_);
        }
        if (out_ != 0) {
            ::SetConsoleOutputCP(out_);
        }
    }

private:
    UINT in_ = 0;
    UINT out_ = 0;
};
#else
class ConsoleUtf8Scope {
public:
    ConsoleUtf8Scope() = default;
};
#endif

}  // namespace

int main(int argc, char** argv) {
    ConsoleUtf8Scope console_utf8;
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
