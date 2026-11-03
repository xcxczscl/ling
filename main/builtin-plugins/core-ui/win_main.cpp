// core-ui 的 desktop 形态：WebView2 独立窗口。
//
// 与 webui 共用 app.cpp 的「握手、自检、起服务」，差别只在：
// 起一个 Win32 窗口、内嵌 WebView2 控件，导航到本机界面地址。
//
// 依赖 WebView2（Windows 10/11 自带运行时，SDK 在 third_party/webview2）。
// 系统里没有 WebView2 运行时时退化成不显示窗口，改由 ling open 走浏览器。
#include <cstdio>
#include <string>

#include "app.h"

#define UNICODE
#define _UNICODE
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <wrl.h>
#include <WebView2.h>

using Microsoft::WRL::Callback;
using Microsoft::WRL::ComPtr;

namespace {

struct DesktopState {
    ling::UiApp* app = nullptr;
    HWND hwnd = nullptr;
    ComPtr<ICoreWebView2Controller> controller;
    ComPtr<ICoreWebView2> webview;
    bool quitting = false;
};

DesktopState g_state;

std::wstring to_wide(const std::string& text) {
    if (text.empty()) {
        return {};
    }
    const int len = ::MultiByteToWideChar(CP_UTF8, 0, text.c_str(), -1, nullptr, 0);
    if (len <= 1) {
        return {};
    }
    std::wstring out(static_cast<std::size_t>(len), L'\0');
    ::MultiByteToWideChar(CP_UTF8, 0, text.c_str(), -1, out.data(), len);
    out.resize(static_cast<std::size_t>(len - 1));
    return out;
}

void resize_webview() {
    if (g_state.controller == nullptr || g_state.hwnd == nullptr) {
        return;
    }
    RECT bounds{};
    ::GetClientRect(g_state.hwnd, &bounds);
    g_state.controller->put_Bounds(bounds);
}

void navigate_to_home() {
    if (g_state.webview == nullptr) {
        return;
    }
    g_state.webview->Navigate(to_wide(g_state.app->url).c_str());
}

// WebView2 环境就绪后创建控制器，再导航。这套是异步回调，靠消息循环驱动。
void init_webview() {
    const std::wstring user_dir =
        to_wide((g_state.app->data_root / "webview2-profile").string());
    const HRESULT hr = CreateCoreWebView2EnvironmentWithOptions(
        nullptr, user_dir.c_str(), nullptr,
        Callback<ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>(
            [](HRESULT result, ICoreWebView2Environment* env) -> HRESULT {
                if (FAILED(result) || env == nullptr) {
                    // 没有 WebView2 运行时。不弹「请安装」的框，只记一句，交给 ling open。
                    std::fprintf(stderr, "没有 WebView2 运行时，改用 ling open 打开界面。\n");
                    return result;
                }
                return env->CreateCoreWebView2Controller(
                    g_state.hwnd,
                    Callback<ICoreWebView2CreateCoreWebView2ControllerCompletedHandler>(
                        [](HRESULT r, ICoreWebView2Controller* ctl) -> HRESULT {
                            if (FAILED(r) || ctl == nullptr) {
                                return r;
                            }
                            g_state.controller = ctl;
                            g_state.controller->get_CoreWebView2(&g_state.webview);
                            // 关掉开发工具；右键菜单沿用 WebView2 的默认菜单，这里不做自定义。
                            if (g_state.webview != nullptr) {
                                ICoreWebView2Settings* settings = nullptr;
                                g_state.webview->get_Settings(&settings);
                                if (settings != nullptr) {
                                    settings->put_AreDevToolsEnabled(FALSE);
                                }
                            }
                            resize_webview();
                            navigate_to_home();
                            return S_OK;
                        })
                        .Get());
            })
            .Get());
    if (FAILED(hr)) {
        std::fprintf(stderr, "WebView2 初始化失败，改用 ling open 打开界面。\n");
    }
}

LRESULT CALLBACK window_proc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    switch (message) {
        case WM_SIZE:
            resize_webview();
            return 0;
        case WM_CLOSE:
            // 关窗口只关界面。要不要顺带退宿主是设置里的选择，desktop 版接到
            // close_behavior 之后再处理，这里先按「只关界面」。
            ::DestroyWindow(hwnd);
            return 0;
        case WM_DESTROY:
            ::PostQuitMessage(0);
            return 0;
        default:
            return ::DefWindowProcW(hwnd, message, wparam, lparam);
    }
}

}  // namespace

int main(int argc, char** argv) {
    // WebView2 要求 STA。
    ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

    ling::UiApp app;
    const int setup = ling::ui_app_setup(argc, argv, &app);
    if (setup != 0) {
        ::CoUninitialize();
        return setup;
    }
    g_state.app = &app;

    HINSTANCE instance = ::GetModuleHandleW(nullptr);
    const wchar_t* class_name = L"LingCoreUiWindow";
    WNDCLASSEXW wc{};
    wc.cbSize = sizeof(wc);
    wc.lpfnWndProc = window_proc;
    wc.hInstance = instance;
    wc.hCursor = ::LoadCursorW(nullptr, IDC_ARROW);
    wc.lpszClassName = class_name;
    ::RegisterClassExW(&wc);

    g_state.hwnd = ::CreateWindowExW(0, class_name, L"ling", WS_OVERLAPPEDWINDOW,
                                     CW_USEDEFAULT, CW_USEDEFAULT, 1100, 760, nullptr, nullptr,
                                     instance, nullptr);
    if (g_state.hwnd == nullptr) {
        ling::ui_app_cleanup(&app);
        ::CoUninitialize();
        return 3;
    }
    ::ShowWindow(g_state.hwnd, SW_SHOW);
    ::UpdateWindow(g_state.hwnd);

    init_webview();

    // 消息循环：WebView2 的回调靠消息泵驱动，界面服务则靠 ui_app_tick。
    MSG msg{};
    while (!g_state.quitting) {
        while (::PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) {
                g_state.quitting = true;
                break;
            }
            ::TranslateMessage(&msg);
            ::DispatchMessageW(&msg);
        }
        if (!ling::ui_app_tick(&app)) {
            break;
        }
    }

    ling::ui_app_cleanup(&app);
    ::CoUninitialize();
    return 0;
}
