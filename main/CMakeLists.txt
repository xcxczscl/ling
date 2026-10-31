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

# 界面进程（core-ui 的侧车产物）。
#
# 现在它以独立可执行文件的形式存在：宿主侧的侧车轨道要到 M4 才做，届时把这个产物
# 装进插件目录、清单里加 [sidecar] 段即可，代码不用重写。
# 页面还没定稿，现阶段只提供 HTTP、认证与 /api/* 转发，见
# docs/ling/builtin-plugins/core-ui/02-服务端与接口.md。
add_executable(ling-ui ui/main.cpp)

target_link_libraries(ling-ui PRIVATE ling_core)

ling_set_standard(ling-ui)
ling_set_warnings(ling-ui)
ling_set_hardening(ling-ui)
ling_set_sanitizer(ling-ui)
