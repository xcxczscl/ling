# 内置插件目录。源码在这里，产物按普通插件路径进 run/<preset>/plugins/，
# 装载时与业务插件不可区分（docs/ling/04-详细设计/07-内置插件.md §9）。
#
# 每个插件在自己的子目录里有 CMakeLists.txt，这一层只负责把它们串起来。
include(${CMAKE_SOURCE_DIR}/tools/cmake/LingPlugin.cmake)

add_subdirectory(core-settings)
add_subdirectory(core-plugin-manager)
add_subdirectory(core-log)
add_subdirectory(core-job)
