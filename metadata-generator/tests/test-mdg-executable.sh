#!/bin/bash
set -e

# Usage: test-mdg-executable.sh <metadata generator executable> [output dir]
MDG=$1
if [ -z "$MDG" ]; then
    echo "Usage: $(basename "$0") <metadata generator executable> [output dir]" 1>&2
    exit 1
fi
MDG="$(cd "$(dirname "$MDG")" && pwd)/$(basename "$MDG")"

TESTSDIR="$(cd "$(dirname "$0")" && pwd)"
ARCH=$(uname -m)
OUTDIR="${2:-$TESTSDIR/TestOutput}"
EXPECTEDOUTPUTDIR="$TESTSDIR/ExpectedOutput-linux-$ARCH"

rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

# clang looks for its builtin headers relative to the executable (./lib/clang)
pushd "$(dirname "$MDG")" > /dev/null
"$MDG" -verbose \
    -input-umbrella "$TESTSDIR/AllSystemFrameworks.h" \
    -output-umbrella "$OUTDIR/umbrella.h" \
    -output-bin "$OUTDIR/metadata-$ARCH.bin" \
    -output-yaml "$OUTDIR/yaml" \
    -output-typescript "$OUTDIR/typings" \
    Xclang \
    -fmodule-map-file="$TESTSDIR/linux/module.modulemap" \
    $(pkg-config --cflags-only-I glib-2.0 2>/dev/null) \
    > "$OUTDIR/verbose.out" 2>&1 || { tail -20 "$OUTDIR/verbose.out"; exit 1; }
popd > /dev/null

grep -E '^Result:|^Done' "$OUTDIR/verbose.out"

if [ ! -d "$EXPECTEDOUTPUTDIR" ]; then
    echo "warning: no expected output at $EXPECTEDOUTPUTDIR, skipping comparison" 1>&2
    exit 0
fi

echo "Comparing test outputs..."
(diff -qwr "$EXPECTEDOUTPUTDIR/yaml" "$OUTDIR/yaml" && echo "Test run successful, no differences encountered.") ||
(echo "error: Metadata generator didn't produce the expected output. Fix or accept the new one by replacing $EXPECTEDOUTPUTDIR with $OUTDIR" 1>&2 && false)
