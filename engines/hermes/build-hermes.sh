#!/bin/bash
# Builds libhermes.a (Hermes + Node-API + js_* host API, one archive) and
# hermesc from microsoft/hermes-windows into vendor/<arch>/ for build.rs.
#
#   HERMES_REV   hermes-windows commit (default: pinned below)
#   HERMES_SRC   existing checkout to build instead of downloading
#   JOBS         parallel jobs (default: nproc)
#   ICU_ROOT     a static ICU install to link instead of the system one; its archives go beside
#                libhermes.a so the runtime doesn't depend on the build distro's ICU soname
set -euo pipefail
cd "$(dirname "$0")"
HERE="$PWD"
HERMES_REV="${HERMES_REV:-5465d9c79f5ada4d9340db01814d191f7b40c417}"
# the revision and this build's patches: vendor/<arch>/REVISION, rebuilt when it changes
STAMP="$HERMES_REV $(cat "$HERE/$(basename "$0")" csrc/* | sha256sum | cut -c1-16)"
if [ "${1:-}" = --stamp ]; then
    echo "$STAMP"
    exit 0
fi
ARCH="$(uname -m)"
WORK="${WORK:-$HERE/target-hermes}"
OUT="$HERE/vendor/$ARCH"
JOBS="${JOBS:-$(nproc)}"
WEAK=""
ICU_ARGS=()
[ -n "${ICU_ROOT:-}" ] && ICU_ARGS=(-DHERMES_USE_STATIC_ICU=ON)
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
JSOBJECT="$SRC/lib/VM/JSObject.cpp"
# an interrupted earlier run can leave it empty, which still compiles
[ -s "$JSOBJECT" ] || { echo "$JSOBJECT is empty: delete $SRC and run again" >&2; exit 1; }
# rewriting it unchanged on reruns would recompile it
grep -q 'getGlobal().get())) {' "$JSOBJECT" &&
    perl -0pi -e 's/LLVM_UNLIKELY\(self == runtime\.objectPrototypeRawPtr\) \|\|\s*LLVM_UNLIKELY\(self == runtime\.getGlobal\(\)\.get\(\)\)/LLVM_UNLIKELY(self == runtime.objectPrototypeRawPtr)/' "$JSOBJECT"
grep -q 'getGlobal().get())) {' "$JSOBJECT" && { echo "global prototype patch failed" >&2; exit 1; }
NODE_API="$SRC/API/hermes_node_api/hermes_node_api.cpp"
# a holder runs finalizers until it is empty, but external ArrayBuffers' finalizers have no
# link back to their holder to leave it: take each one out before running it
grep -q 'const auto &ref = finalizers_.begin();' "$NODE_API" &&
    perl -0pi -e 's/const auto &ref = finalizers_\.begin\(\);\n(\s*)\(\*ref\)->invokeFinalizerFromGC\(\);/NodeApiReference *ref = *finalizers_.begin();\n$1finalizers_.erase(finalizers_.begin());\n$1ref->invokeFinalizerFromGC();/' "$NODE_API"
grep -q 'const auto &ref = finalizers_.begin();' "$NODE_API" && { echo "finalizer holder patch failed" >&2; exit 1; }
# finalizers of collected napi_wrap objects only run when GC happens inside a Node-API scope,
# else they wait for the environment's shutdown: let the host run them from its event loop
grep -q runPendingFinalizers "$NODE_API" || cat >> "$NODE_API" <<'CPP'

namespace hermes::node_api {
napi_status runPendingFinalizers(napi_env env) noexcept {
  return CHECKED_ENV(env)->processFinalizerQueueFromCode();
}
} // namespace hermes::node_api
CPP
mkdir -p "$SRC/API/hermes_ns"
cp csrc/CMakeLists.txt csrc/hermes_jsr.cpp "$SRC/API/hermes_ns/"
grep -q hermes_ns "$SRC/API/CMakeLists.txt" || echo 'add_subdirectory(hermes_ns)' >> "$SRC/API/CMakeLists.txt"

BUILD="$WORK/build-$ARCH"
# PIC: ns-linux is PIE, and not every toolchain (manylinux's gcc-toolset) defaults to it
cmake -S "$SRC" -B "$BUILD" -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DHERMES_ENABLE_DEBUGGER=OFF -DHERMES_ENABLE_INTL=OFF \
    -DHERMES_ENABLE_TEST_SUITE=OFF -DHERMES_BUILD_APPLE_FRAMEWORK=OFF "${ICU_ARGS[@]}"
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
rm -f "$OUT"/libicu*.a
[ -n "${ICU_ROOT:-}" ] && cp "$ICU_ROOT"/lib/libicu{i18n,uc,data}.a "$OUT/"
strip "$OUT/hermesc"
echo "$STAMP" > "$OUT/REVISION"
ls -la "$OUT"
