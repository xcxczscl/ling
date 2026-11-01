# core-ui 的构建：侧车插件（可执行文件）。宿主拉起它、走 jsonl 帧握手
# （docs/ling/04-详细设计/02-插件装载与接口.md §8）。
#
# 产物按普通插件布局放，安装到数据根后与业务插件不可区分（04-详细设计/07 §9）。
# 页面在 page.html，构建时转成字符串嵌进 exe：发布只有一个文件，
# 也不怕磁盘上的前端文件被删（docs/ling/builtin-plugins/core-ui/02 §3.1）。
add_custom_command(
    OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc"
    COMMAND python "${CMAKE_SOURCE_DIR}/tools/embed.py"
            "${CMAKE_CURRENT_SOURCE_DIR}/page.html"
            "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/page.html"
            "${CMAKE_SOURCE_DIR}/tools/embed.py"
    COMMENT "把页面嵌进 core-ui")

# 两种形态：默认 webui（控制台，浏览器打开）；LING_CORE_UI_DESKTOP=ON 时编成
# WebView2 窗口（GUI 子系统，不弹黑框）。两者共用 app.cpp 的握手与起服务。
option(LING_CORE_UI_DESKTOP "core-ui 编成 WebView2 窗口形态（仅 Windows）" OFF)

set(CORE_UI_SOURCES app.cpp page.cpp ui_server.cpp
    "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc")
if(LING_CORE_UI_DESKTOP AND WIN32)
    list(APPEND CORE_UI_SOURCES win_main.cpp)
    add_executable(core-ui ${CORE_UI_SOURCES})
    # WebView2.h 与 WRL 的 Callback 需要 Windows SDK 的 winrt 头；手设 INCLUDE 环境变量
    # 不含它。版本号随 SDK 升级变化，用 glob 找最新版而不是硬编码，避免换 SDK 就断。
    file(GLOB WINDOWS_SDK_WINRT_DIRS
        "C:/Program Files (x86)/Windows Kits/10/Include/*/winrt")
    list(SORT WINDOWS_SDK_WINRT_DIRS)
    if(WINDOWS_SDK_WINRT_DIRS)
        list(GET WINDOWS_SDK_WINRT_DIRS -1 WINDOWS_SDK_WINRT_DIR)
    else()
        message(FATAL_ERROR "找不到 Windows SDK 的 winrt 头目录（WebView2 需要），请装 Windows SDK")
    endif()
    target_include_directories(core-ui PRIVATE
        "${CMAKE_CURRENT_BINARY_DIR}"
        "${CMAKE_SOURCE_DIR}/third_party/webview2/build/native/include"
        "${WINDOWS_SDK_WINRT_DIR}")
    target_link_libraries(core-ui PRIVATE ling_core
        "${CMAKE_SOURCE_DIR}/third_party/webview2/build/native/x64/WebView2LoaderStatic.lib"
        user32 ole32 shlwapi version)
    # GUI 子系统不弹控制台；但入口仍是 main，所以显式指定 CRT 启动函数。
    set_target_properties(core-ui PROPERTIES WIN32_EXECUTABLE TRUE)
    target_link_options(core-ui PRIVATE "/ENTRY:mainCRTStartup")
else()
    list(APPEND CORE_UI_SOURCES main.cpp)
    add_executable(core-ui ${CORE_UI_SOURCES})
    target_include_directories(core-ui PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
    target_link_libraries(core-ui PRIVATE ling_core)
endif()

ling_set_standard(core-ui)
ling_set_warnings(core-ui)
ling_set_hardening(core-ui)
ling_set_sanitizer(core-ui)

set_target_properties(core-ui PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${LING_PLUGIN_OUTPUT_DIR}/core-ui")
add_custom_command(TARGET core-ui POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "${CMAKE_CURRENT_SOURCE_DIR}/plugin.toml"
            "${LING_PLUGIN_OUTPUT_DIR}/core-ui/plugin.toml")
