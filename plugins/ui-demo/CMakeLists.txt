# 本插件的构建声明。加一个插件只需在自己的目录里写这一行，外面不用改。
add_ling_plugin(ui-demo "${CMAKE_CURRENT_SOURCE_DIR}")

# 面板文件不嵌入，运行时由 core-ui 从 <插件目录>/ui 读。
# 这里跟着产物一起复制，`ling install run/<preset>/plugins/ui-demo` 才有东西可读。
add_custom_command(TARGET ui-demo POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_directory
            "${CMAKE_CURRENT_SOURCE_DIR}/ui"
            "${LING_PLUGIN_OUTPUT_DIR}/ui-demo/ui")
