# 业务插件目录。每个插件在自己的子目录里有 CMakeLists.txt，这里只负责把它们串起来。
include(${CMAKE_SOURCE_DIR}/tools/cmake/LingPlugin.cmake)

add_subdirectory(demo-a)
add_subdirectory(demo-b)
add_subdirectory(demo-caller)
add_subdirectory(demo-bench)
add_subdirectory(demo-sidecar)

# Python 插件不编译：源文件直接摆到产物目录，装的时候与动态库插件长得一样。
add_custom_target(copy-demo-py ALL
    COMMAND ${CMAKE_COMMAND} -E copy_directory
            "${CMAKE_CURRENT_SOURCE_DIR}/demo-py"
            "${LING_PLUGIN_OUTPUT_DIR}/demo-py"
    COMMENT "复制 Python 示例插件")
