# 插件构建。业务插件源码在 plugins/<编号>/，内置插件源码在 main/builtin-plugins/<编号>/，
# 两者的产物都落到 run/<preset>/plugins/<编号>/，装载时不可区分（04/07 §9.3）。
include(${CMAKE_SOURCE_DIR}/tools/cmake/LingPlugin.cmake)

add_ling_plugin(demo-a)
add_ling_plugin(demo-caller)

# 内置插件：MVP 三个，core-ui 与 core-job 待 M5。
add_ling_plugin(core-settings "${CMAKE_SOURCE_DIR}/main/builtin-plugins/core-settings")
add_ling_plugin(core-plugin-manager "${CMAKE_SOURCE_DIR}/main/builtin-plugins/core-plugin-manager")
add_ling_plugin(core-log "${CMAKE_SOURCE_DIR}/main/builtin-plugins/core-log")
