# core-ui 的构建：侧车插件（可执行文件）。宿主拉起它、走 jsonl 帧握手
# （docs/ling/04-详细设计/02-插件装载与接口.md §8）。
#
# 产物按普通插件布局放，安装到数据根后与业务插件不可区分（04-详细设计/07 §9）。
# 外壳的前端文件构建期由 tools/embed.py 转成常量表嵌进 exe：发布只有两个文件
# （exe + plugin.toml），也不怕磁盘上的前端文件被删
# （docs/ling/builtin-plugins/core-ui/03-架构设计.md §3.1）。
# 面板的资源不嵌入，运行时从各插件目录读（04-详细设计/01 §1）。

set(CORE_UI_WEB "${CMAKE_CURRENT_SOURCE_DIR}")

# 一个键给多个文件时按顺序拼起来：三个脚本分开写、审代码时各看各的，
# 发出去仍是同一个 /shell.js（路由表里只有这一个脚本地址）。
set(CORE_UI_ASSETS
    "shell.html=${CORE_UI_WEB}/shell.html"
    "shell.css=${CORE_UI_WEB}/shell.css"
    "shell.js=${CORE_UI_WEB}/theme.js,${CORE_UI_WEB}/panels.js,${CORE_UI_WEB}/shell.js"
    "views/plugins.js=${CORE_UI_WEB}/views/plugins.js"
    "views/logs.js=${CORE_UI_WEB}/views/logs.js"
    "views/config.js=${CORE_UI_WEB}/views/config.js"
    "views/jobs.js=${CORE_UI_WEB}/views/jobs.js"
    "views/appearance.js=${CORE_UI_WEB}/views/appearance.js"
    "views/about.js=${CORE_UI_WEB}/views/about.js")

set(CORE_UI_ASSET_FILES
    shell.html shell.css theme.js panels.js shell.js
    views/plugins.js views/logs.js views/config.js
    views/jobs.js views/appearance.js views/about.js)

foreach(asset ${CORE_UI_ASSET_FILES})
    list(APPEND CORE_UI_ASSET_PATHS "${CORE_UI_WEB}/${asset}")
endforeach()

add_custom_command(
    OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/ui_assets.inc"
    COMMAND python "${CMAKE_SOURCE_DIR}/tools/embed.py"
            "${CMAKE_CURRENT_BINARY_DIR}/ui_assets.inc"
            ${CORE_UI_ASSETS}
    DEPENDS ${CORE_UI_ASSET_PATHS} "${CMAKE_SOURCE_DIR}/tools/embed.py"
    COMMENT "把外壳页面、样式与脚本嵌进 core-ui")

# 两种形态：默认 webui（控制台，浏览器打开）；LING_CORE_UI_DESKTOP=ON 时编成
# WebView2 窗口（GUI 子系统）。两者共用 app.cpp 的握手与起服务
# （docs/ling/builtin-plugins/core-ui/04-详细设计/05-进程形态与启动.md §2）。
option(LING_CORE_UI_DESKTOP "core-ui 编成 WebView2 窗口形态（仅 Windows）" OFF)

set(CORE_UI_SOURCES
    app.cpp
    ui_server.cpp
    src/assets.cpp
    src/error_copy.cpp
    src/panel_assets.cpp
    src/panel_registry.cpp
    src/theme.cpp
    "${CMAKE_CURRENT_BINARY_DIR}/ui_assets.inc")

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
    # 供「关于」页判断「关窗口时退出 ling」这项能不能用。
    target_compile_definitions(core-ui PRIVATE LING_CORE_UI_DESKTOP_BUILD=1)
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
