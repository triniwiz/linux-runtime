# Release build images, used by scripts/release-build-portable.sh.
#
#   portable  manylinux_2_28 (AlmaLinux 8, glibc 2.28): every shipped binary is built here, so
#             the packages run on any glibc distro since 2018
#   sdk       Ubuntu 24.04: only the GTK / GLib headers the SDK metadata is generated from, and
#             the libjavascriptcoregtk-6.0 the jsc engine links against
ARG ARCH=x86_64

FROM quay.io/pypa/manylinux_2_28_${ARCH} AS portable
RUN dnf install -y -q --enablerepo=powertools glib2-devel zlib-static libuuid-devel clang lld rsync file \
    && dnf clean all
RUN /opt/python/cp312-cp312/bin/pip install -q --root-user-action=ignore ninja \
    && ln -s /opt/python/cp312-cp312/bin/ninja /usr/local/bin/
# Static ICU for Hermes: a dynamic one would tie linux-hermes to the build distro's ICU soname
RUN curl -fsSL https://github.com/unicode-org/icu/releases/download/release-74-2/icu4c-74_2-src.tgz \
        | tar -xz -C /tmp \
    && cd /tmp/icu/source \
    && CFLAGS="-O2 -fPIC" CXXFLAGS="-O2 -fPIC" ./configure -q --prefix=/opt/icu --enable-static \
        --disable-shared --disable-tests --disable-samples --with-data-packaging=static \
    && make -s -j"$(nproc)" install \
    && rm -rf /tmp/icu
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH ICU_ROOT=/opt/icu
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain stable
RUN case "$(uname -m)" in x86_64) a=x64 ;; aarch64) a=arm64 ;; esac \
    && curl -fsSL "https://nodejs.org/dist/v22.21.1/node-v22.21.1-linux-$a.tar.xz" \
        | tar -xJ -C /usr/local --strip-components=1
RUN git config --global --add safe.directory '*'

FROM ubuntu:24.04 AS sdk
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -q && apt-get install -y -q --no-install-recommends \
        build-essential python3 pkg-config libglib2.0-dev libgtk-4-dev libgtk-3-dev libgles-dev libegl-dev \
        libxml2-dev libicu-dev uuid-dev zlib1g-dev libfontconfig-dev libpango1.0-dev \
        libjavascriptcoregtk-6.0-dev \
    && rm -rf /var/lib/apt/lists/*
