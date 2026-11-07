#!/bin/bash
# Builds llvm-project 17.0.6 (clang libraries) into llvm/<version>/{include,lib_<arch>}.
set -e
source "$(dirname "$0")/build_utils.sh"

LLVM_VERSION="17.0.6"
LLVM_TAG="llvmorg-$LLVM_VERSION"

ARCH="${LLVM_ARCH:-$(uname -m)}"
ROOT="$(cd "$(dirname "$0")" && pwd)"
WORK_DIR="${LLVM_WORK_DIR:-/tmp/ns-linux-llvm}"
SRC_DIR="$WORK_DIR/llvm-project-$LLVM_TAG"
BUILD_DIR="$WORK_DIR/build-$LLVM_TAG-$ARCH"
STAGE_DIR="$WORK_DIR/install-$LLVM_TAG-$ARCH"
OUT_DIR="$ROOT/llvm/$LLVM_VERSION"
NUMJOBS=${NUMJOBS:-$(nproc 2>/dev/null || echo 1)}

case "$ARCH" in
    x86_64) TARGETS="X86;AArch64" ;;
    aarch64|arm64) TARGETS="AArch64;X86" ;;
    *) TARGETS="host" ;;
esac

if [ -d "$OUT_DIR/lib_$ARCH" ]; then
    echo "llvm $LLVM_VERSION for $ARCH already present at $OUT_DIR/lib_$ARCH"
    exit 0
fi

mkdir -p "$WORK_DIR"

if [ ! -d "$SRC_DIR" ]; then
    checkpoint "Cloning llvm/llvm-project ($LLVM_TAG)..."
    git clone --depth 1 --branch "$LLVM_TAG" https://github.com/llvm/llvm-project.git "$SRC_DIR"
fi

GENERATOR="Unix Makefiles"
if command -v ninja >/dev/null; then
    GENERATOR="Ninja"
fi

LINKER_ARGS=()
if command -v ld.lld >/dev/null; then
    LINKER_ARGS=(-DLLVM_USE_LINKER=lld)
fi

checkpoint "Configuring llvm $LLVM_VERSION for $ARCH..."
cmake -S "$SRC_DIR/llvm" -B "$BUILD_DIR" -G "$GENERATOR" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$STAGE_DIR" \
    -DLLVM_ENABLE_PROJECTS="clang" \
    -DLLVM_ENABLE_RUNTIMES="" \
    -DLLVM_TARGETS_TO_BUILD="$TARGETS" \
    -DLLVM_ENABLE_ASSERTIONS=OFF \
    -DLLVM_ENABLE_RTTI=OFF \
    -DBUILD_SHARED_LIBS=OFF \
    -DLLVM_ENABLE_ZSTD=OFF \
    -DLLVM_ENABLE_ZLIB=ON \
    -DLLVM_ENABLE_TERMINFO=OFF \
    -DLLVM_ENABLE_LIBXML2=OFF \
    -DLLVM_ENABLE_LIBEDIT=OFF \
    -DLLVM_ENABLE_BINDINGS=OFF \
    -DLLVM_BUILD_TOOLS=OFF \
    -DCLANG_BUILD_TOOLS=OFF \
    -DLLVM_INCLUDE_TESTS=OFF \
    -DLLVM_INCLUDE_BENCHMARKS=OFF \
    -DLLVM_INCLUDE_EXAMPLES=OFF \
    -DLLVM_INCLUDE_DOCS=OFF \
    -DCLANG_INCLUDE_TESTS=OFF \
    -DCLANG_INCLUDE_DOCS=OFF \
    -DCLANG_ENABLE_ARCMT=OFF \
    -DCLANG_ENABLE_STATIC_ANALYZER=ON \
    -DLLVM_PARALLEL_LINK_JOBS=4 \
    "${LINKER_ARGS[@]}"

checkpoint "Building llvm $LLVM_VERSION for $ARCH ($NUMJOBS jobs)..."
cmake --build "$BUILD_DIR" -j "$NUMJOBS"
# llvm-config is excluded from 'all' when LLVM_BUILD_TOOLS=OFF, but it's handy
# for the generator's CMake to query link flags
cmake --build "$BUILD_DIR" -j "$NUMJOBS" --target llvm-config

checkpoint "Installing llvm $LLVM_VERSION for $ARCH..."
rm -rf "$STAGE_DIR"
cmake --install "$BUILD_DIR"

mkdir -p "$OUT_DIR"
rm -rf "$OUT_DIR/lib_$ARCH" "$OUT_DIR/bin_$ARCH"
mv "$STAGE_DIR/lib" "$OUT_DIR/lib_$ARCH"
mkdir -p "$OUT_DIR/bin_$ARCH"
cp "$BUILD_DIR/bin/llvm-config" "$OUT_DIR/bin_$ARCH/"
if [ ! -d "$OUT_DIR/include" ]; then
    mv "$STAGE_DIR/include" "$OUT_DIR/include"
fi

checkpoint "Done: $OUT_DIR"
