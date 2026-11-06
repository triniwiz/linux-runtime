#!/bin/bash
# Builds the runtime npm packages consumed by the NativeScript CLI (`ns run linux`).
#
# Usage: scripts/pack-npm.sh [version]
#   METADATA=<metadata.bin dir>  default: metadata-generator/tests/TestOutput
#   TYPINGS=<typings dir>        default: $METADATA/typings
#
# Layout (see nativescript-cli lib/services/linux-project-service.ts):
#   framework/__PROJECT_NAME__/app
#   framework/runtime/<x64|arm64>/ns-linux, metadata-<x86_64|aarch64>.bin
#   framework/typings/*.d.ts
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "nativescript-runtime"))')}"
METADATA="${METADATA:-metadata-generator/tests/TestOutput}"
TYPINGS="${TYPINGS:-$METADATA/typings}"
RUST_ARCH="$(uname -m)"
case "$RUST_ARCH" in
    x86_64) ARCH=x64 ;;
    aarch64) ARCH=arm64 ;;
    *) echo "unsupported architecture $RUST_ARCH" >&2; exit 1 ;;
esac
[ -f "$METADATA/metadata-$RUST_ARCH.bin" ] || { echo "missing $METADATA/metadata-$RUST_ARCH.bin" >&2; exit 1; }

cargo build --release -p nativescript-cli -p nativescript-quickjs

pack() {
    local name=$1 binary=$2 engine=$3
    local out="dist/npm/$name"
    rm -rf "$out"
    mkdir -p "$out/framework/runtime/$ARCH" "$out/framework/typings"
    cp -r npm/template/. "$out/framework/"
    install -m 755 "target/release/$binary" "$out/framework/runtime/$ARCH/ns-linux"
    strip "$out/framework/runtime/$ARCH/ns-linux"
    cp "$METADATA/metadata-$RUST_ARCH.bin" "$out/framework/runtime/$ARCH/"
    cp "$TYPINGS"/*.d.ts "$out/framework/typings/" 2>/dev/null || true
    cat > "$out/package.json" <<JSON
{
  "name": "@nativescript/$name",
  "version": "$VERSION",
  "description": "NativeScript Linux runtime ($engine)",
  "license": "Apache-2.0",
  "repository": "github:NativeScript/linux",
  "keywords": ["NativeScript", "Linux", "GTK"],
  "os": ["linux"],
  "files": ["framework"]
}
JSON
    (cd "$out" && npm pack --silent --pack-destination ..)
}

pack linux ns-linux "V8"
pack linux-quickjs ns-linux-quickjs "QuickJS, Node-API"
ls -la dist/npm/*.tgz
