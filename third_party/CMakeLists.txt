cmake_minimum_required(VERSION 3.14)
project(dsterm_llama C)

add_subdirectory(llama.cpp)

add_library(dsterm_shim STATIC
    llama.cpp/dsterm_shim.c
)

target_include_directories(dsterm_shim PRIVATE
    llama.cpp/include
)

set_target_properties(dsterm_shim PROPERTIES
    C_STANDARD 11
    C_STANDARD_REQUIRED ON
)

target_link_libraries(dsterm_shim PRIVATE llama)

install(TARGETS dsterm_shim ARCHIVE DESTINATION lib)
