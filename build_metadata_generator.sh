#!/bin/bash
set -e
source "$(dirname "$0")/build_utils.sh"

"$(dirname "$0")/build_llvm.sh"

# try to build in the amount of threads available
# change this to 1 if you want single threaded builds
NUMJOBS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 1)
ARCH="${METADATA_BINARY_ARCH:-$(uname -m)}"

function build {
    rm -rf build
    cmake -B build -DCMAKE_BUILD_TYPE=Release -DMETADATA_BINARY_ARCH=$1
    cmake --build build -j $NUMJOBS
    mkdir -p "dist/$1"
    cp -r "build/bin" "dist/$1"
}

pushd "metadata-generator"
rm -rf dist
mkdir dist

checkpoint "Building metadata generator for $ARCH ..."
build "$ARCH"
rm -rf build
popd
