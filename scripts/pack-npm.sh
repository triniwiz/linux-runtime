#!/bin/bash
# Builds the runtime npm packages consumed by the NativeScript CLI (`ns run linux`).
#
# Usage: scripts/pack-npm.sh [version]
#   METADATA=<metadata.bin dir>  default: metadata-generator/tests/TestOutput
#   TYPINGS=<typings dir>        default: $METADATA/typings
#   MDG=<generator bin dir>      metadata-generator + lib/clang, for app/plugin native code
#   ENGINES="quickjs v8 jsc hermes napi"  packages besides @nativescript/linux (V8): the
#                                Node-API engines (jsc needs libjavascriptcoregtk, hermes
#                                engines/hermes/build-hermes.sh) and napi, the linux-napi addon
#   STRICT=1                     fail instead of skipping an engine that can't be built (CI)
#
# Layout (see nativescript-cli lib/services/linux-project-service.ts):
#   framework/__PROJECT_NAME__/app
#   framework/runtime/<x64|arm64>/ns-linux, metadata-<x86_64|aarch64>.bin
#   framework/typings/*.d.ts
#   framework/tools/<x64|arm64>/metadata-generator, lib/clang/<version>/include
#   framework/tools/<x64|arm64>/ns-metadata-filter   native-api-usage.json filtering
#   framework/tools/<x64|arm64>/ns-bundle            app.nsbundle packing, runtime fuses
#   framework/tools/<x64|arm64>/hermesc              Hermes bytecode compiler (linux-hermes)
# linux-napi: linux.<x64|arm64>.node, metadata-<x86_64|aarch64>.bin, typings/
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nativescript-runtime"))')}"
METADATA="${METADATA:-metadata-generator/tests/TestOutput}"
TYPINGS="${TYPINGS:-$METADATA/typings}"
MDG="${MDG:-metadata-generator/dist/$(uname -m)/bin}"
RUST_ARCH="$(uname -m)"
case "$RUST_ARCH" in
    x86_64) ARCH=x64 ;;
    aarch64) ARCH=arm64 ;;
    *) echo "unsupported architecture $RUST_ARCH" >&2; exit 1 ;;
esac
[ -f "$METADATA/metadata-$RUST_ARCH.bin" ] || { echo "missing $METADATA/metadata-$RUST_ARCH.bin" >&2; exit 1; }

ENGINES="${ENGINES-quickjs v8 jsc hermes napi}"
BUILT=()
skip() {
    echo "warning: skipping linux-$1, $2" >&2
    [ -z "${STRICT:-}" ] || exit 1
}
for e in $ENGINES; do
    case $e in
        hermes) [ -f "${NS_HERMES_DIR:-engines/hermes/vendor/$RUST_ARCH}/libhermes.a" ] ||
            { skip hermes "run engines/hermes/build-hermes.sh"; continue; } ;;
        napi)
            # its own workspace (nativescript-napi without the engine feature)
            if (cd linux-napi && cargo build --release); then BUILT+=(napi); else skip napi "its build failed"; fi
            continue ;;
    esac
    if cargo build --release -p "nativescript-$e"; then
        BUILT+=("$e")
    else
        skip "$e" "its build failed"
    fi
done
cargo build --release -p nativescript-cli -p nativescript-metadata -p nativescript-core

pack() {
    local name=$1 binary=$2 engine=$3 id=$4
    local out="dist/npm/$name"
    rm -rf "$out"
    mkdir -p "$out/framework/runtime/$ARCH" "$out/framework/typings"
    cp -r npm/template/. "$out/framework/"
    install -m 755 "target/release/$binary" "$out/framework/runtime/$ARCH/ns-linux"
    strip "$out/framework/runtime/$ARCH/ns-linux"
    cp "$METADATA/metadata-$RUST_ARCH.bin" "$out/framework/runtime/$ARCH/"
    cp "$TYPINGS"/*.d.ts npm/typings/linux-runtime.d.ts "$out/framework/typings/" 2>/dev/null || true
    # GUI stack only: libc / icu declarations clash with other platforms' typings
    for m in GLib Gtk pango cairo gdk_pixbuf graphene harfbuzz fontconfig; do
        [ -f "$out/framework/typings/c!$m.d.ts" ] && echo "/// <reference path=\"./c!$m.d.ts\" />"
    done > "$out/framework/typings/index.d.ts"
    echo '/// <reference path="./linux-runtime.d.ts" />' >> "$out/framework/typings/index.d.ts"
    mkdir -p "$out/framework/tools/$ARCH"
    for tool in ns-metadata-filter ns-bundle; do
        install -m 755 "target/release/$tool" "$out/framework/tools/$ARCH/"
        strip "$out/framework/tools/$ARCH/$tool"
    done
    if [ -x "$MDG/metadata-generator" ]; then
        mkdir -p "$out/framework/tools/$ARCH/lib"
        install -m 755 "$MDG/metadata-generator" "$out/framework/tools/$ARCH/"
        strip "$out/framework/tools/$ARCH/metadata-generator"
        cp -r "$MDG/lib/clang" "$out/framework/tools/$ARCH/lib/"
    else
        echo "warning: no metadata generator in $MDG, app/plugin native code won't build" >&2
        [ -z "${STRICT:-}" ] || exit 1
    fi
    if [ "$id" = hermes ]; then
        install -m 755 target/release/hermesc "$out/framework/tools/$ARCH/"
    fi
    cat > "$out/package.json" <<JSON
{
  "name": "@nativescript/$name",
  "version": "$VERSION",
  "description": "NativeScript Linux runtime ($engine)",
  "license": "Apache-2.0",
  "repository": "github:NativeScript/linux",
  "keywords": ["NativeScript", "Linux", "GTK"],
  "os": ["linux"],
  "files": ["framework"],
  "nativescript": { "runtime": "linux", "engine": "$id" }
}
JSON
    (cd "$out" && npm pack --silent --pack-destination ..)
}

pack_addon() {
    local out="dist/npm/linux-napi"
    rm -rf "$out"
    mkdir -p "$out/typings"
    cp linux-napi/index.js linux-napi/index.d.ts "$out/"
    [ -f linux-napi/README.md ] && cp linux-napi/README.md "$out/"
    install -m 755 linux-napi/target/release/liblinux_napi.so "$out/linux.$ARCH.node"
    strip "$out/linux.$ARCH.node"
    cp "$METADATA/metadata-$RUST_ARCH.bin" "$out/"
    cp "$TYPINGS"/*.d.ts npm/typings/linux-runtime.d.ts "$out/typings/" 2>/dev/null || true
    for m in GLib Gtk pango cairo gdk_pixbuf graphene harfbuzz fontconfig; do
        [ -f "$out/typings/c!$m.d.ts" ] && echo "/// <reference path=\"./c!$m.d.ts\" />"
    done > "$out/typings/index.d.ts"
    echo '/// <reference path="./linux-runtime.d.ts" />' >> "$out/typings/index.d.ts"
    node -e '
      const fs = require("fs");
      const [src, dst, version] = process.argv.slice(1);
      const pkg = JSON.parse(fs.readFileSync(src, "utf8"));
      pkg.version = version;
      delete pkg.scripts;
      fs.writeFileSync(dst, JSON.stringify(pkg, null, 2) + "\n");
    ' linux-napi/package.json "$out/package.json" "$VERSION"
    (cd "$out" && npm pack --silent --pack-destination ..)
}

pack linux ns-linux "V8" v8
for e in "${BUILT[@]}"; do
    case $e in
        quickjs) pack linux-quickjs ns-linux-quickjs "QuickJS, Node-API" quickjs ;;
        v8) pack linux-v8 ns-linux-v8 "V8 over Node-API" v8 ;;
        jsc) pack linux-jsc ns-linux-jsc "JavaScriptCore, Node-API; needs WebKitGTK's libjavascriptcoregtk" jsc ;;
        hermes) pack linux-hermes ns-linux-hermes "Hermes, Node-API" hermes ;;
        napi) pack_addon ;;
    esac
done
ls -la dist/npm/*.tgz
