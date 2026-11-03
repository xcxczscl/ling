include_directories(${CMAKE_SOURCE_DIR}/main/include)

add_executable(ling_unit
    unit/test_manifest.cpp
    unit/test_router.cpp
    unit/test_event.cpp
    unit/test_depgraph.cpp
    unit/test_sidecar_proto.cpp
    unit/test_sys.cpp
)

target_link_libraries(ling_unit PRIVATE ling_core ling_catch2)

ling_set_standard(ling_unit)
ling_set_warnings(ling_unit)
ling_set_hardening(ling_unit)
ling_set_sanitizer(ling_unit)

set_target_properties(ling_unit PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/tests")

add_test(NAME unit COMMAND ling_unit)

# 侧车管道层的手工探针：起真的侧车进程、走一遍握手与调用。
# 不进 ctest——它需要一个已构建好的侧车产物，路径由调用者给。
add_executable(ling_sidecar_probe manual/sidecar_probe.cpp)
target_link_libraries(ling_sidecar_probe PRIVATE ling_core ling_json)
ling_set_standard(ling_sidecar_probe)
ling_set_warnings(ling_sidecar_probe)
ling_set_hardening(ling_sidecar_probe)
ling_set_sanitizer(ling_sidecar_probe)
set_target_properties(ling_sidecar_probe PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/tests")
