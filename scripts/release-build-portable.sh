#!/bin/bash
# Builds every npm package for the host's architecture against glibc 2.28 and writes
# dist/npm-<x64|arm64>.tar.gz; merge the two arches like npm_release.yml's `pack` job.
# Runs on any docker host (Linux or Docker Desktop), the build happens in the images from
# scripts/release.Dockerfile, in a docker volume so caches (LLVM, Hermes, cargo) survive.
#
# Usage: scripts/release-build-portable.sh <version>      e.g. 0.1.0-alpha.3
#   VOLUME=...    build volume (default: ns-release-portable-<arch>)
#   ENGINES=...   default: quickjs v8 jsc hermes napi (see pack-npm.sh)
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:?usage: $0 <version>}"
case "$(uname -m)" in
    x86_64 | amd64) ARCH=x86_64 NPM_ARCH=x64 ;;
    aarch64 | arm64) ARCH=aarch64 NPM_ARCH=arm64 ;;
    *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
VOLUME="${VOLUME:-ns-release-portable-$ARCH}"
ENGINES="${ENGINES-quickjs v8 jsc hermes napi}"
GLIBC_MAX=2.28

for target in portable sdk; do
    docker build -q --build-arg ARCH="$ARCH" --target "$target" -t "ns-linux-$target:$ARCH" \
        -f scripts/release.Dockerfile scripts
done
run() {
    local image=$1
    shift
    docker run --rm -i -v "$VOLUME:/src" -w /src -e CARGO_HOME=/src/target/cargo-home "ns-linux-$image:$ARCH" bash -euo pipefail -c "$@"
}

if git rev-parse --git-dir >/dev/null 2>&1; then
    git ls-files -co --exclude-standard -z
else
    find . \( -path ./target -o -path ./dist -o -path ./llvm -o -name node_modules \) -prune \
        -o -type f -print0
fi | COPYFILE_DISABLE=1 tar --no-xattrs --null -T - -cf - | run portable 'tar -xf - -C /src'

# LLVM is built once into /src/llvm; its work dir is several GB and only needed for that
run portable '
    export LLVM_WORK_DIR=/src/target/llvm-work
    ./build_metadata_generator.sh
    rm -rf "$LLVM_WORK_DIR"
    if [[ " '"$ENGINES"' " == *" hermes "* ]] && [ ! -f engines/hermes/vendor/'"$ARCH"'/libicuuc.a ]; then
        engines/hermes/build-hermes.sh
    fi'

# metadata comes from the 24.04 GTK headers; the generator and binaries stay glibc 2.28
run sdk '
    MDG=metadata-generator/dist/'"$ARCH"'/bin/metadata-generator
    metadata-generator/tests/test-mdg-executable.sh "$MDG"
    NS_SDK_CONFIG="$PWD/metadata-generator/linux/sdk-gtk3.json" \
        metadata-generator/tests/test-mdg-executable.sh "$MDG" "$PWD/metadata-generator/tests/TestOutput-gtk3"
    mkdir -p target/jsc-link
    cp -L /usr/lib/'"$ARCH"'-linux-gnu/libjavascriptcoregtk-6.0.so.1 target/jsc-link/'

# The jsc engine links the 24.04 libjavascriptcoregtk-6.0, whose own dependencies are newer
# than this image's: --allow-shlib-undefined leaves resolving them to the user's system.
run portable '
    export ENGINES="'"$ENGINES"'" STRICT=1 NS_JSC_LIB=/src/target/jsc-link/libjavascriptcoregtk-6.0.so.1
    export RUSTFLAGS="-C link-arg=-fuse-ld=lld -C link-arg=-Wl,--allow-shlib-undefined"
    scripts/pack-npm.sh "'"$VERSION"'"
    dirs=(linux)
    for e in $ENGINES; do dirs+=("linux-$e"); done
    status=0
    while IFS= read -r f; do
        file -b "$f" | grep -q "^ELF" || continue
        v=$(objdump -T "$f" | grep -o "GLIBC_[0-9.]*" | sed "s/GLIBC_//" | sort -uV | tail -1)
        if [ "$(printf "%s\n%s\n" "$v" '"$GLIBC_MAX"' | sort -V | tail -1)" != '"$GLIBC_MAX"' ]; then
            echo "error: $f needs glibc $v" >&2
            status=1
        fi
        if objdump -p "$f" | grep -q "NEEDED.*libicu"; then
            echo "error: $f links the build host ICU" >&2
            status=1
        fi
    done < <(cd dist/npm && find "${dirs[@]}" -type f -perm -u+x -o -type f -name "*.node" | sed "s|^|dist/npm/|")
    [ $status = 0 ]
    tar -czf dist/npm-'"$NPM_ARCH"'.tar.gz -C dist/npm "${dirs[@]}"'

mkdir -p dist
run portable 'cat dist/npm-'"$NPM_ARCH"'.tar.gz' > "dist/npm-$NPM_ARCH.tar.gz"
echo "built dist/npm-$NPM_ARCH.tar.gz (glibc <= $GLIBC_MAX)"
