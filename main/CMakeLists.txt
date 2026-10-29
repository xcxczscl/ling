# 宿主：内核静态库 + 命令行入口。
#
# 静态库而不是动态库的理由见 docs/ling/05-构造与开发指南.md §3.1：
# 宿主与插件不该各自带一份内核代码，静态库在链接期把用到的部分合进去，
# 没有装载期开销。

add_library(ling_core STATIC
    src/version.cpp
)

target_include_directories(ling_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)

ling_set_standard(ling_core)
ling_set_warnings(ling_core)
ling_set_hardening(ling_core)

# 版本号来自顶层 CMakeLists，编进库里而不是让每个用到的地方自己拼。
target_compile_definitions(ling_core PRIVATE LING_VERSION="${LING_VERSION}")

add_executable(ling app/main.cpp)

target_link_libraries(ling PRIVATE ling_core)

ling_set_standard(ling)
ling_set_warnings(ling)
ling_set_hardening(ling)
