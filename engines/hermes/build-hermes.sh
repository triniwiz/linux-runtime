#!/bin/bash
# Builds libhermes.a (Hermes + Node-API + js_* host API, one archive) and
# hermesc from microsoft/hermes-windows into vendor/<arch>/ for build.rs.
#
#   HERMES_REV   hermes-windows commit (default: pinned below)
#   HERMES_SRC   existing checkout to build instead of downloading
#   JOBS         parallel jobs (default: nproc)
set -euo pipefail
cd "$(dirname "$0")"
HERE="$PWD"
HERMES_REV="${HERMES_REV:-5465d9c79f5ada4d9340db01814d191f7b40c417}"
ARCH="$(uname -m)"
WORK="${WORK:-$HERE/target-hermes}"
OUT="$HERE/vendor/$ARCH"
JOBS="${JOBS:-$(nproc)}"
WEAK=""
mkdir -p "$WORK" "$OUT"

SRC="${HERMES_SRC:-$WORK/hermes-windows-$HERMES_REV}"
if [ ! -d "$SRC" ]; then
    curl -fL "https://codeload.github.com/microsoft/hermes-windows/tar.gz/$HERMES_REV" -o "$WORK/hermes.tar.gz"
    mkdir -p "$SRC"
    tar xzf "$WORK/hermes.tar.gz" -C "$SRC" --strip-components=1
    rm "$WORK/hermes.tar.gz"
fi

# CMake modules are case sensitive off Windows
sed -i 's/^include (ctest)/include(CTest)/' "$SRC/cmake/modules/Lit.cmake"
# metadata globals resolve through a Proxy in the global's prototype chain
perl -0pi -e 's/LLVM_UNLIKELY\(self == runtime\.objectPrototypeRawPtr\) \|\|\s*LLVM_UNLIKELY\(self == runtime\.getGlobal\(\)\.get\(\)\)/LLVM_UNLIKELY(self == runtime.objectPrototypeRawPtr)/' "$SRC/lib/VM/JSObject.cpp"
grep -q 'getGlobal().get())) {' "$SRC/lib/VM/JSObject.cpp" && { echo "global prototype patch failed" >&2; exit 1; }
mkdir -p "$SRC/API/hermes_ns"
cp csrc/CMakeLists.txt csrc/hermes_jsr.cpp "$SRC/API/hermes_ns/"
grep -q hermes_ns "$SRC/API/CMakeLists.txt" || echo 'add_subdirectory(hermes_ns)' >> "$SRC/API/CMakeLists.txt"

BUILD="$WORK/build-$ARCH"
cmake -S "$SRC" -B "$BUILD" -DCMAKE_BUILD_TYPE=Release \
    -DHERMES_ENABLE_DEBUGGER=OFF -DHERMES_ENABLE_INTL=OFF \
    -DHERMES_ENABLE_TEST_SUITE=OFF -DHERMES_BUILD_APPLE_FRAMEWORK=OFF
cmake --build "$BUILD" --target hermes_ns hermesc -j "$JOBS"

# one archive for build.rs
LIB="$OUT/libhermes.a"
rm -f "$LIB"
{
    echo "CREATE $LIB"
    for a in API/hermes_ns/libhermes_ns.a lib/libhermesvm_a.a API/hermes_node_api/libhermesNodeApi.a \
        jsi/libjsi.a external/boost/boost_1_86_0/libs/context/libboost_context.a; do
        [ -f "$BUILD/$a" ] && echo "ADDLIB $BUILD/$a"
    done
    echo "SAVE"
    echo "END"
} | ar -M
# node_api.h functions nativescript-napi's compat also defines: its own win
for sym in $(grep -ohE 'extern "C" fn (napi|node_api)_[a-z0-9_]+' "$HERE/../../napi/src/compat.rs" | awk '{print $4}'); do
    WEAK+=" -W $sym"
done
objcopy $WEAK "$LIB"
strip --strip-debug "$LIB"
install -m 755 "$BUILD/bin/hermesc" "$OUT/hermesc"
strip "$OUT/hermesc"
echo "$HERMES_REV" > "$OUT/REVISION"
ls -la "$OUT"
