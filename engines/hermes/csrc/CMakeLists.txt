# The js_* host API over Hermes' Node-API (copied into hermes-windows' API/ by
# build-hermes.sh, which merges it with its dependencies into libhermes.a).
add_library(hermes_ns STATIC hermes_jsr.cpp)
target_include_directories(hermes_ns PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/..)
target_link_libraries(hermes_ns PUBLIC nodeApi hermesNodeApi hermesvm_a jsi)
# match Hermes' -fno-rtti (TaskRunner's typeinfo isn't emitted)
target_compile_options(hermes_ns PRIVATE -fno-rtti -fno-exceptions)
