cmake_minimum_required(VERSION 3.20)
project(MetadataGenerator)
include(CheckCCompilerFlag)
include(CheckCXXCompilerFlag)

IF(NOT DEFINED METADATA_BINARY_ARCH)
    set(METADATA_BINARY_ARCH "${CMAKE_HOST_SYSTEM_PROCESSOR}")
ENDIF(NOT DEFINED METADATA_BINARY_ARCH)

# Apple docset comments in the typings (iOS); off keeps the binary free of libxml2/ICU
option(NS_DOCSETS "Read Apple docsets with libxml2" OFF)
if (NS_DOCSETS)
    find_package(LibXml2 REQUIRED)
    add_compile_definitions(NS_DOCSETS)
endif ()

# Same LLVM (Apple's llvm-project fork) the NativeScript iOS runtime uses,
# built for Linux by ../build_llvm.sh
set(LLVM_VERSION "17.0.6")
get_filename_component(LLVM_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../llvm/${LLVM_VERSION}" REALPATH)
set(LLVM_LIBDIR "${LLVM_ROOT}/lib_${METADATA_BINARY_ARCH}")
if (NOT EXISTS "${LLVM_LIBDIR}")
    message(FATAL_ERROR "LLVM ${LLVM_VERSION} not found at ${LLVM_LIBDIR}, run ./build_llvm.sh first")
endif ()

set(LLVM_PREPROCESSOR_FLAGS "-isystem ${LLVM_ROOT}/include -D__STDC_CONSTANT_MACROS -D__STDC_FORMAT_MACROS -D__STDC_LIMIT_MACROS")

# Link every static clang/LLVM archive inside a group so we don't have to
# maintain a hand-ordered list of libraries like the iOS build does.
file(GLOB LLVM_STATIC_LIBS "${LLVM_LIBDIR}/libclang*.a" "${LLVM_LIBDIR}/libLLVM*.a")
list(FILTER LLVM_STATIC_LIBS EXCLUDE REGEX ".*/libclang\\.a$")
# zlib, libstdc++ and libgcc linked statically: the binary only needs glibc
find_library(ZLIB_STATIC NAMES libz.a)
if (NOT ZLIB_STATIC)
    set(ZLIB_STATIC z)
endif ()
set(LLVM_LINK_LIBS -Wl,--start-group ${LLVM_STATIC_LIBS} -Wl,--end-group ${ZLIB_STATIC} m pthread dl)
add_link_options(-static-libstdc++ -static-libgcc)

set(CMAKE_BUILD_TYPE Release)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -fno-rtti ${LLVM_PREPROCESSOR_FLAGS}")

check_cxx_compiler_flag(-fuse-ld=lld HAS_LLD)
if (HAS_LLD)
    add_link_options(-fuse-ld=lld)
endif()

# Warnings triggered from inside LLVM's own headers
foreach(flag unused-but-set-variable deprecated-enum-enum-conversion class-memaccess
        init-list-lifetime redundant-move pessimizing-move maybe-uninitialized)
    string(MAKE_C_IDENTIFIER "HAS_NO_${flag}" _var)
    check_cxx_compiler_flag(-Wno-${flag} ${_var})
    if (${_var})
        add_compile_options(-Wno-${flag})
    endif()
endforeach()

set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin/lib)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin/lib)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

# Output executables in the bin/ directory (not in subdirs) because clang libs
# are hardcoded to search for their built-in headers in `./lib`
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY_DEBUG ${CMAKE_RUNTIME_OUTPUT_DIRECTORY})
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY_RELWITHDEBINFO ${CMAKE_RUNTIME_OUTPUT_DIRECTORY})
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY_MINSIZEREL ${CMAKE_RUNTIME_OUTPUT_DIRECTORY})
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY_RELEASE ${CMAKE_RUNTIME_OUTPUT_DIRECTORY})

add_subdirectory(src)
