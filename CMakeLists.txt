cmake_minimum_required(VERSION 3.25)

# 版本号见 docs/common/02-版本号与兼容约定.md §2：
#   大版本.首要版本.次版本-b<构建序号>
# bN 是打包轮次，每次构建产物递增；功能变化才动次版本。
# 0.0.3 对应 M2（可装可卸），也就是 MVP 的交付版本（11-MVP 范围.md §1.1）。
set(LING_VERSION_MAJOR 0)
set(LING_VERSION_MINOR 0)
set(LING_VERSION_PATCH 3)
set(LING_BUILD 1)
set(LING_VERSION
    "${LING_VERSION_MAJOR}.${LING_VERSION_MINOR}.${LING_VERSION_PATCH}-b${LING_BUILD}")

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
