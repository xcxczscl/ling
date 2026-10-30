# 插件目标统一入口。依据 docs/ling/05-构造与开发指南.md 的约定：
# 插件是动态库，产物名等于插件编号，输出到 run/<preset>/plugins/<编号>/。
#
# 用法：
#   add_ling_plugin(<编号>)              源码在 plugins/<编号>/
#   add_ling_plugin(<编号> <源码目录>)   内置插件用，源码在 main/builtin-plugins/<编号>/
function(add_ling_plugin plugin_id)
    if(ARGC GREATER 1)
        set(plugin_dir "${ARGV1}")
    else()
        set(plugin_dir "${CMAKE_SOURCE_DIR}/plugins/${plugin_id}")
    endif()

    set(source "${plugin_dir}/main.cpp")
    if(NOT EXISTS "${source}")
        message(FATAL_ERROR "插件 ${plugin_id} 缺少 main.cpp（${plugin_dir}）")
    endif()

    add_library(${plugin_id} SHARED "${source}")
    target_include_directories(${plugin_id} PRIVATE "${CMAKE_SOURCE_DIR}/main/include")
    # 插件也要解析 JSON：配置、日志、方法参数都是 JSON 文本。
    target_link_libraries(${plugin_id} PRIVATE ling_plugin_api ling_json)

    ling_set_standard(${plugin_id})
    ling_set_warnings(${plugin_id})
    ling_set_hardening(${plugin_id})
    ling_set_sanitizer(${plugin_id})

    # 产物名必须等于编号：装载器按 <编号>.dll / <编号>.so 找文件。
    set_target_properties(${plugin_id} PROPERTIES
        OUTPUT_NAME "${plugin_id}"
        LIBRARY_OUTPUT_DIRECTORY "${LING_PLUGIN_OUTPUT_DIR}/${plugin_id}"
        RUNTIME_OUTPUT_DIRECTORY "${LING_PLUGIN_OUTPUT_DIR}/${plugin_id}")

    # 清单随产物一起落到运行目录，方便直接 ling install。
    add_custom_command(TARGET ${plugin_id} POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${plugin_dir}/plugin.toml"
                "${LING_PLUGIN_OUTPUT_DIR}/${plugin_id}/plugin.toml")
endfunction()
