# 业务插件目录。每个插件在自己的子目录里有 CMakeLists.txt，这里只负责把它们串起来。
include(${CMAKE_SOURCE_DIR}/tools/cmake/LingPlugin.cmake)

add_subdirectory(demo-a)
add_subdirectory(demo-caller)
add_subdirectory(demo-bench)
