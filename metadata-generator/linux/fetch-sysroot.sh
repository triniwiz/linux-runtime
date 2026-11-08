#!/bin/bash
# Extracts -dev packages (and their pkg-config deps) into a sysroot without root:
#   fetch-sysroot.sh /tmp/ns-sysroot [debs...] [--pkg gtk4]
# Then: NS_SYSROOT=/tmp/ns-sysroot gen-sdk.py --out ...
set -e
SYSROOT=${1:?usage: fetch-sysroot.sh <sysroot> [deb packages...] [--pkg name...]}
shift
DEBS=()
PKGS=()
while [ $# -gt 0 ]; do
    if [ "$1" = "--pkg" ]; then PKGS+=("$2"); shift 2; else DEBS+=("$1"); shift; fi
done
[ ${#DEBS[@]} -eq 0 ] && DEBS=(libgtk-4-dev libglib2.0-dev libgles-dev libegl-dev)
[ ${#PKGS[@]} -eq 0 ] && PKGS=(gtk4 glesv2 egl)

DL="$SYSROOT.debs"
mkdir -p "$SYSROOT" "$DL"
cd "$DL"
apt-get download "${DEBS[@]}"

declare -A MAP=([libjpeg]=libjpeg-turbo8-dev [libtiff-4]=libtiff-dev [xcb]=libxcb1-dev [x11]=libx11-dev
    [egl]=libegl-dev [gl]=libgl-dev [glesv2]=libgles-dev [libpcre]=libpcre3-dev [libpcre2-8]=libpcre2-dev [uuid]=uuid-dev
    [xproto]=x11proto-dev [kbproto]=x11proto-dev [renderproto]=x11proto-dev [xextproto]=x11proto-dev
    [icu-uc]=libicu-dev [wayland-protocols]=wayland-protocols [xi]=libxi-dev [zlib]=zlib1g-dev
    [libffi]=libffi-dev [pixman-1]=libpixman-1-dev [freetype2]=libfreetype-dev [fontconfig]=libfontconfig-dev
    [harfbuzz]=libharfbuzz-dev [pango]=libpango1.0-dev [cairo]=libcairo2-dev [gdk-pixbuf-2.0]=libgdk-pixbuf-2.0-dev
    [graphene-gobject-1.0]=libgraphene-1.0-dev [xcb-shm]=libxcb-shm0-dev [xcb-render]=libxcb-render0-dev
    [libsysprof-capture-4]=libsysprof-4-dev [mount]=libmount-dev [blkid]=libblkid-dev [libselinux]=libselinux1-dev
    [libsepol]=libsepol-dev [expat]=libexpat1-dev [libbrotlidec]=libbrotli-dev [xrender]=libxrender-dev [xext]=libxext-dev [libpng16]=libpng-dev
    [wayland-client]=libwayland-dev [wayland-cursor]=libwayland-dev [wayland-egl]=libwayland-dev [wayland-server]=libwayland-dev
    [wayland-scanner]=libwayland-bin [xkbcommon]=libxkbcommon-dev [epoxy]=libepoxy-dev [xfixes]=libxfixes-dev [xrandr]=libxrandr-dev)

host_pc=$(pkg-config --variable pc_path pkg-config)
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_LIBDIR=$(echo "$host_pc" | tr ':' '\n' | sed "s|^|$SYSROOT|" | paste -sd:):$host_pc

for _ in $(seq 1 60); do
    for d in *.deb; do dpkg-deb -x "$d" "$SYSROOT"; done
    miss=$(pkg-config --cflags "${PKGS[@]}" 2>&1 | grep -oP "Package '\K[^']+(?=', required by)|Package \K\S+(?= was not found)" | head -1 || true)
    if [ -z "$miss" ]; then
        echo "sysroot ready: $SYSROOT"
        exit 0
    fi
    # pkg-config name to -dev package: datrie-0.2 -> libdatrie-dev, libthai -> libthai-dev
    base=$(echo "$miss" | sed -E "s/-[0-9.]+$//")
    case "$base" in lib*) guess=$base-dev ;; *) guess=lib$base-dev ;; esac
    deb=${MAP[$miss]:-$guess}
    echo "resolving $miss -> $deb"
    apt-get download "$deb"
done
echo "giving up resolving dependencies" >&2
exit 1
