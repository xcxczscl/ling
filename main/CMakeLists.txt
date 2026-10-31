# 宿主：内核静态库 + 命令行入口 + 插件边界头文件。
#
# 静态库而不是动态库的理由见 docs/ling/05-构造与开发指南.md §3.1：
# 宿主与插件不该各自带一份内核代码，静态库在链接期把用到的部分合进去，
# 没有装载期开销。

# 插件边界：宿主与插件都只需要这一个 include 目录，其中只有 plugin_api.h 是契约。
add_library(ling_plugin_api INTERFACE)
target_include_directories(ling_plugin_api INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/include)

add_library(ling_core STATIC
    src/version.cpp
    src/manifest.cpp
    src/depgraph.cpp
    src/sidecar_proto.cpp
    src/sidecar_host.cpp
    src/router.cpp
    src/ledger.cpp
    src/event.cpp
    src/kernel.cpp
    src/admin_server.cpp
    src/admin_client.cpp
    src/ui_server.cpp
    src/log.cpp
)

# 管理端口用裸套接字，Windows 上必须显式链 ws2_32。
if(WIN32)
    target_link_libraries(ling_core PUBLIC ws2_32)
endif()

target_include_directories(ling_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)

# 版本号来自顶层 CMakeLists，编进库里而不是让每个用到的地方自己拼。
target_compile_definitions(ling_core PRIVATE
    LING_VERSION="${LING_VERSION}"
    LING_HAS_TOMLPP=1)
target_link_libraries(ling_core PUBLIC ling_toml ling_json)

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
# 页面在 main/ui/page.html，构建时转成字符串嵌进 exe：发布只有一个文件，
# 也不怕磁盘上的前端文件被删（core-ui/02 §3.1）。
add_custom_command(
    OUTPUT "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc"
    COMMAND python "${CMAKE_SOURCE_DIR}/tools/embed.py"
            "${CMAKE_CURRENT_SOURCE_DIR}/ui/page.html"
            "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc"
    DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/ui/page.html"
            "${CMAKE_SOURCE_DIR}/tools/embed.py"
    COMMENT "把页面嵌进 core-ui")

add_executable(core-ui ui/main.cpp ui/page.cpp "${CMAKE_CURRENT_BINARY_DIR}/page_html.inc")
target_include_directories(core-ui PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")

target_link_libraries(core-ui PRIVATE ling_core)

ling_set_standard(core-ui)
ling_set_warnings(core-ui)
ling_set_hardening(core-ui)
ling_set_sanitizer(core-ui)

set_target_properties(core-ui PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${LING_PLUGIN_OUTPUT_DIR}/core-ui")
add_custom_command(TARGET core-ui POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
            "${CMAKE_CURRENT_SOURCE_DIR}/ui/plugin.toml"
            "${LING_PLUGIN_OUTPUT_DIR}/core-ui/plugin.toml")

# Python 侧车 SDK 随宿主分发。插件经 LING_SDK_DIR 找到它，不必各自拷一份。
add_custom_target(copy-python-sdk ALL
    COMMAND ${CMAKE_COMMAND} -E copy_directory
            "${CMAKE_CURRENT_SOURCE_DIR}/python"
            "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/python"
    COMMENT "复制 Python 侧车 SDK")
