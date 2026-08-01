cmake_minimum_required(VERSION 3.14)
project(dsterm_llama C CXX)

# Deterministic values for common/build-info.cpp.in (the vendored tree has no
# .git; these only appear in log lines, so hardcoding the tag is fine).
set(LLAMA_BUILD_NUMBER 10210)
set(LLAMA_BUILD_COMMIT "b10210")
set(BUILD_COMPILER "unknown")
set(BUILD_TARGET "unknown")

add_subdirectory(llama.cpp)

#
# dsterm_common: llama.cpp's native chat-template engine (common/chat.cpp and
# its transitive closure, incl. the bundled Jinja2 engine). The `llama`
# target does not include any of this, and upstream's full llama-common pulls
# in arg/console/download/... that dsterm does not need. The list below is
# the closure discovered at link time for the 4 chat functions dsterm uses;
# if the link ever regresses, bisect unresolved symbols against
# common/CMakeLists.txt's llama-common source list.
#

configure_file(llama.cpp/common/build-info.cpp.in
               ${CMAKE_CURRENT_BINARY_DIR}/build-info.cpp @ONLY)

add_library(dsterm_common STATIC
    ${CMAKE_CURRENT_BINARY_DIR}/build-info.cpp
    llama.cpp/common/chat.cpp
    llama.cpp/common/chat-auto-parser-generator.cpp
    llama.cpp/common/chat-auto-parser-helpers.cpp
    llama.cpp/common/chat-diff-analyzer.cpp
    llama.cpp/common/chat-peg-parser.cpp
    llama.cpp/common/common.cpp
    llama.cpp/common/fit.cpp
    llama.cpp/common/json-schema-to-grammar.cpp
    llama.cpp/common/log.cpp
    llama.cpp/common/peg-parser.cpp
    llama.cpp/common/reasoning-budget.cpp
    llama.cpp/common/sampling.cpp
    llama.cpp/common/trie.cpp
    llama.cpp/common/unicode.cpp
    llama.cpp/common/jinja/lexer.cpp
    llama.cpp/common/jinja/parser.cpp
    llama.cpp/common/jinja/runtime.cpp
    llama.cpp/common/jinja/value.cpp
    llama.cpp/common/jinja/string.cpp
    llama.cpp/common/jinja/caps.cpp
)

target_include_directories(dsterm_common PUBLIC
    llama.cpp/common
    llama.cpp/vendor
)

set_target_properties(dsterm_common PROPERTIES
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
)

target_link_libraries(dsterm_common PUBLIC llama)

install(TARGETS dsterm_common ARCHIVE DESTINATION lib)

#
# dsterm_shim: stable C API over llama.cpp for dsterm's Rust FFI, plus the
# chat-template C++ shim (dsterm_shim_chat.cpp).
#

add_library(dsterm_shim STATIC
    llama.cpp/dsterm_shim.c
    llama.cpp/dsterm_shim_chat.cpp
)

target_include_directories(dsterm_shim PRIVATE
    llama.cpp/include
    llama.cpp/common
    llama.cpp/vendor
)

set_target_properties(dsterm_shim PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED ON
    CXX_STANDARD 17
    CXX_STANDARD_REQUIRED ON
)

target_link_libraries(dsterm_shim PRIVATE dsterm_common llama)

install(TARGETS dsterm_shim ARCHIVE DESTINATION lib)
