include_directories(${CMAKE_SOURCE_DIR}/main/include)

add_executable(ling_unit
    unit/test_manifest.cpp
    unit/test_router.cpp
    unit/test_event.cpp
    unit/test_depgraph.cpp
    unit/test_sidecar_proto.cpp
)

target_link_libraries(ling_unit PRIVATE ling_core ling_catch2)

ling_set_standard(ling_unit)
ling_set_warnings(ling_unit)
ling_set_hardening(ling_unit)
ling_set_sanitizer(ling_unit)

set_target_properties(ling_unit PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/tests")

add_test(NAME unit COMMAND ling_unit)
