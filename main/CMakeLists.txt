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

# 内核层（src/kernel/）：四项服务 + 三个机制，见 docs/ling/03-架构设计.md §2.1。
# 通道层（src/channel/）：管理端口 + 侧车协议。平台相关的装载通道实现在 Sys/。
add_library(ling_core STATIC
    src/kernel/version.cpp
    src/kernel/manifest.cpp
    src/kernel/depgraph.cpp
    src/kernel/gate.cpp
    src/kernel/router.cpp
    src/kernel/ledger.cpp
    src/kernel/event.cpp
    src/kernel/kernel.cpp
    src/kernel/log.cpp
    src/channel/admin_server.cpp
    src/channel/admin_client.cpp
    src/channel/sidecar_proto.cpp
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
