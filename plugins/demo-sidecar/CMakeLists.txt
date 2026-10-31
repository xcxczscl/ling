# 侧车插件的构建声明。产物是可执行文件，进 run/<preset>/plugins/demo-sidecar/。
include(${CMAKE_SOURCE_DIR}/tools/cmake/LingPlugin.cmake)
add_ling_sidecar(demo-sidecar "${CMAKE_CURRENT_SOURCE_DIR}")
