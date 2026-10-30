# 第三方依赖：三个都取上游单文件发行版，版本与校验和记在 third_party/README.md。
# 不用 vcpkg 的原因见同文件：本机沙箱拒绝 vcpkg 写 %LOCALAPPDATA%，且 S1 只需三个库。

add_library(ling_toml INTERFACE)
target_include_directories(ling_toml INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/tomlplusplus)

add_library(ling_json INTERFACE)
target_include_directories(ling_json INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/nlohmann)

# Catch2 v3 的合并版：一个头文件加一个实现文件，编成静态库只编一次。
add_library(ling_catch2 STATIC ${CMAKE_CURRENT_SOURCE_DIR}/catch2/catch_amalgamated.cpp)
target_include_directories(ling_catch2 PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/catch2)
ling_set_standard(ling_catch2)
# 必须和测试目标用同一个消毒器开关：ASan 会关掉 MSVC STL 的容器注解，
# 两侧取值不同会触发 LNK2038（annotate_string / annotate_vector 不匹配）。
ling_set_sanitizer(ling_catch2)
# 必须和测试目标用同一个消毒器开关：ASan 会关掉 MSVC STL 的容器注解，
# 两侧取值不同会触发 LNK2038（annotate_string / annotate_vector 不匹配）。
ling_set_sanitizer(ling_catch2)
