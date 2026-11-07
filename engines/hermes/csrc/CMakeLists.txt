# libhermes.so: Hermes + its Node-API + the js_* host API (copied into
# hermes-windows' API/ by build-hermes.sh).
add_library(hermes_ns SHARED hermes_jsr.cpp)
target_include_directories(hermes_ns PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/..)
target_link_libraries(hermes_ns PUBLIC nodeApi PRIVATE hermesNodeApi hermesvm_a jsi)
# match Hermes' -fno-rtti (TaskRunner's typeinfo isn't emitted)
target_compile_options(hermes_ns PRIVATE -fno-rtti -fno-exceptions)
target_link_options(hermes_ns PRIVATE -Wl,--no-undefined ${HERMES_EXTRA_LINKER_FLAGS})
set_target_properties(hermes_ns PROPERTIES OUTPUT_NAME hermes)
