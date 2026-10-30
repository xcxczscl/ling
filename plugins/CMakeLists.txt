# 业务插件。S1 只放两个示例：demo-a 提供方法，demo-caller 调它。
# 每个插件一个子目录，源码固定 main.cpp，清单 plugin.toml 与源码同目录。
include(${CMAKE_SOURCE_DIR}/tools/cmake/LingPlugin.cmake)

add_ling_plugin(demo-a)
add_ling_plugin(demo-caller)
