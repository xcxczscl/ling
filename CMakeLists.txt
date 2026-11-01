cmake_minimum_required(VERSION 3.25)

# 版本号见 docs/common/02-版本号与兼容约定.md §2：
#   大版本.首要版本.次版本-b<构建序号>
# 开发期用 bN 标记构建轮次；项目宣告完成后去掉 -bN 成为正式版。
# 0.1.0 是正式版（M6 已通过、项目宣告完成，10-路线图与里程碑.md §3.7、§4）。
set(LING_VERSION_MAJOR 0)
set(LING_VERSION_MINOR 1)
set(LING_VERSION_PATCH 0)
set(LING_VERSION "${LING_VERSION_MAJOR}.${LING_VERSION_MINOR}.${LING_VERSION_PATCH}")

project(ling VERSION ${LING_VERSION_MAJOR}.${LING_VERSION_MINOR}.${LING_VERSION_PATCH}
        DESCRIPTION "插件宿主 + 功能插件" LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# 产物落点：run/<preset>/，由 Presets 传入 LING_PRESET；未走 Presets 时退回构建类型。
if(NOT LING_PRESET)
    set(LING_PRESET "${CMAKE_BUILD_TYPE}")
endif()
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_SOURCE_DIR}/run/${LING_PRESET}")
set(LING_PLUGIN_OUTPUT_DIR "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/plugins")

# 统一编译选项（docs/ling/05-构造与开发指南.md §3.2）。所有目标都必须调用这三个函数。
list(APPEND CMAKE_MODULE_PATH "${CMAKE_SOURCE_DIR}/tools/cmake")
include(LingOptions)

enable_testing()

add_subdirectory(third_party)
add_subdirectory(main)
add_subdirectory(plugins)
add_subdirectory(tests)

message(STATUS "ling ${LING_VERSION} | preset=${LING_PRESET} | 输出目录=${CMAKE_RUNTIME_OUTPUT_DIRECTORY}")
