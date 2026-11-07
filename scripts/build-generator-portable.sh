#!/bin/bash
# Builds the metadata generator against glibc 2.28 (manylinux_2_28, AlmaLinux 8) so
# one binary runs on every glibc distro since 2018: Debian 10+, Ubuntu 20.04+,
# Fedora 29+, RHEL / Alma / Rocky 8+, openSUSE Leap 15.1+, Arch.
# It only links libc and libm (zlib, libstdc++ and libgcc are static).
#
# Usage: ARCH=x86_64|aarch64 scripts/build-generator-portable.sh   (needs podman or docker;
# a foreign ARCH needs qemu binfmt). Output: metadata-generator/dist/<arch>/bin
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="${ARCH:-$(uname -m)}"
case "$ARCH" in
    x86_64) PLATFORM=linux/amd64 ;;
    aarch64) PLATFORM=linux/arm64 ;;
    *) echo "unsupported ARCH $ARCH" >&2; exit 1 ;;
esac
ENGINE="$(command -v podman || command -v docker || true)"
[ -n "$ENGINE" ] || { echo "needs podman or docker" >&2; exit 1; }

"$ENGINE" run --rm --platform "$PLATFORM" \
    -v "$PWD:/src" -w /src \
    -e LLVM_WORK_DIR=/src/target/llvm-work -e METADATA_BINARY_ARCH="$ARCH" \
    "quay.io/pypa/manylinux_2_28_$ARCH" bash -euo pipefail -c '
        dnf install -y -q --enablerepo=powertools zlib-static >/dev/null
        /opt/python/cp312-cp312/bin/pip install -q cmake ninja
        export PATH=/opt/python/cp312-cp312/bin:$PATH
        git config --global --add safe.directory "*"
        ./build_metadata_generator.sh
        # the glibc the binary needs, for the record
        objdump -T metadata-generator/dist/'"$ARCH"'/bin/metadata-generator | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -1
    '
