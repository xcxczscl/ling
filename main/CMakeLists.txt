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
    src/router.cpp
    src/kernel.cpp
    src/log.cpp
)

target_include_directories(ling_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)

# 版本号来自顶层 CMakeLists，编进库里而不是让每个用到的地方自己拼。
target_compile_definitions(ling_core PRIVATE
    LING_VERSION="${LING_VERSION}"
    LING_HAS_TOMLPP=1)
target_link_libraries(ling_core PRIVATE ling_toml)

ling_set_standard(ling_core)
ling_set_warnings(ling_core)
ling_set_hardening(ling_core)
ling_set_sanitizer(ling_core)

add_executable(ling app/main.cpp)

target_link_libraries(ling PRIVATE ling_core)

ling_set_standard(ling)
ling_set_warnings(ling)
ling_set_hardening(ling)
ling_set_sanitizer(ling)
