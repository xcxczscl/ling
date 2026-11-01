# 宿主：内核静态库 + 命令行入口 + 插件边界头文件。
#
# 静态库而不是动态库的理由见 docs/ling/05-构造与开发指南.md §3.1：
# 宿主与插件不该各自带一份内核代码，静态库在链接期把用到的部分合进去，
# 没有装载期开销。

# 插件边界：宿主与插件都只需要这一个 include 目录，其中只有 plugin_api.h 是契约。
add_library(ling_plugin_api INTERFACE)
target_include_directories(ling_plugin_api INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/include)

# 平台抽象层：纯平台函数（Sys/Windows/sys.cpp 或 Sys/Posix/sys.cpp）。
# 宿主内核（ling_core）与进程内插件都要链接它——插件是独立 DLL，不链 ling_core，
# 但也要用 local_time、atomic_replace 这类平台能力，所以单独成一个库。
add_library(ling_sys STATIC)
if(WIN32)
    target_sources(ling_sys PRIVATE Sys/Windows/sys.cpp)
    target_link_libraries(ling_sys PUBLIC ws2_32)  # Winsock 初始化（WSAStartup）在 sys.cpp
else()
    target_sources(ling_sys PRIVATE Sys/Posix/sys.cpp)
    # dlopen/dlsym/dlclose 在较老的 glibc（< 2.34）上要显式链 -ldl。
    target_link_libraries(ling_sys PUBLIC ${CMAKE_DL_LIBS})
endif()
target_include_directories(ling_sys PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)
ling_set_standard(ling_sys)
ling_set_warnings(ling_sys)
ling_set_hardening(ling_sys)
ling_set_sanitizer(ling_sys)

add_library(ling_core STATIC
    src/version.cpp
    src/manifest.cpp
    src/depgraph.cpp
    src/sidecar_proto.cpp
    src/router.cpp
    src/ledger.cpp
    src/event.cpp
    src/kernel.cpp
    src/admin_server.cpp
    src/admin_client.cpp
    src/ui_server.cpp
    src/log.cpp
)

# sidecar_host 的进程实现（Sidecar 类）是宿主内核的一部分，按平台只编一份。
if(WIN32)
    target_sources(ling_core PRIVATE Sys/Windows/sidecar_host.cpp)
else()
    target_sources(ling_core PRIVATE Sys/Posix/sidecar_host.cpp)
endif()

target_include_directories(ling_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)

# 版本号来自顶层 CMakeLists，编进库里而不是让每个用到的地方自己拼。
target_compile_definitions(ling_core PRIVATE
    LING_VERSION="${LING_VERSION}"
    LING_HAS_TOMLPP=1)
target_link_libraries(ling_core PUBLIC ling_sys ling_toml ling_json)

ling_set_standard(ling_core)
ling_set_warnings(ling_core)
ling_set_hardening(ling_core)
ling_set_sanitizer(ling_core)

# 内置插件：源码在 main/builtin-plugins/，产物进 run/<preset>/plugins/。
add_subdirectory(builtin-plugins)

add_executable(ling app/main.cpp)

target_link_libraries(ling PRIVATE ling_core)

ling_set_standard(ling)
ling_set_warnings(ling)
ling_set_hardening(ling)
ling_set_sanitizer(ling)

# 界面插件 core-ui。它是侧车：宿主拉起它、走 jsonl 帧握手（docs/ling/04-详细设计/02 §8）。
# 产物按普通插件布局放，安装到数据根后与业务插件不可区分（04/07 §9）。
# 页面在 builtin-plugins/core-ui/page.html，构建时转成字符串嵌进 exe：发布只有一个文件，
# 也不怕磁盘上的前端文件被删（core-ui/02 §3.1）。
add_custom_command(
    OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc"
    COMMAND python "${CMAKE_SOURCE_DIR}/tools/embed.py"
            "${CMAKE_CURRENT_SOURCE_DIR}/builtin-plugins/core-ui/page.html"
            "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/builtin-plugins/core-ui/page.html"
            "${CMAKE_SOURCE_DIR}/tools/embed.py"
    COMMENT "把页面嵌进 core-ui")

# 两种形态：默认 webui（控制台，浏览器打开）；LING_CORE_UI_DESKTOP=ON 时编成
# WebView2 窗口（GUI 子系统，不弹黑框）。两者共用 app.cpp 的握手与起服务。
option(LING_CORE_UI_DESKTOP "core-ui 编成 WebView2 窗口形态（仅 Windows）" OFF)

set(CORE_UI_SOURCES builtin-plugins/core-ui/app.cpp builtin-plugins/core-ui/page.cpp
    "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc")
if(LING_CORE_UI_DESKTOP AND WIN32)
    list(APPEND CORE_UI_SOURCES builtin-plugins/core-ui/win_main.cpp)
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
    list(APPEND CORE_UI_SOURCES builtin-plugins/core-ui/main.cpp)
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
            "${CMAKE_CURRENT_SOURCE_DIR}/builtin-plugins/core-ui/plugin.toml"
            "${LING_PLUGIN_OUTPUT_DIR}/core-ui/plugin.toml")

# Python 侧车 SDK 随宿主分发。插件经 LING_SDK_DIR 找到它，不必各自拷一份。
add_custom_target(copy-python-sdk ALL
    COMMAND ${CMAKE_COMMAND} -E copy_directory
            "${CMAKE_CURRENT_SOURCE_DIR}/SDK/python"
            "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/python"
    COMMENT "复制 Python 侧车 SDK")

# Python 嵌入运行时随宿主分发：宿主按 <程序目录>/runtime/python/python.exe 找它，
# Python 插件因此脱离系统环境（M6）。目前只打包 Windows x64。
if(WIN32)
    add_custom_target(copy-python-runtime ALL
        COMMAND ${CMAKE_COMMAND} -E copy_directory
                "${CMAKE_SOURCE_DIR}/third_party/python-embed"
                "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/runtime/python"
        COMMENT "复制 Python 嵌入运行时")
endif()
