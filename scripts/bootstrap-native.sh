#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$project_dir/.local-native"
version="1.3.0"
archive_url="https://github.com/wmww/gtk4-layer-shell/archive/refs/tags/v${version}.tar.gz"
archive_sha256="1ebb01ab14e98afd1727f68f64981c37bd23305b1f131f5667c02b94cf593192"

if PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
    pkg-config --exists 'gtk4-layer-shell-0 >= 1'; then
    printf '%s\n' "gtk4-layer-shell is already available in $prefix"
    exit 0
fi

for command_name in curl meson ninja pkg-config sha256sum tar; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf '%s\n' "Missing required build tool: $command_name" >&2
        exit 1
    fi
done

build_root=$(mktemp -d "${TMPDIR:-/tmp}/chathead-native.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

archive="$build_root/gtk4-layer-shell.tar.gz"
curl -fL "$archive_url" -o "$archive"
printf '%s  %s\n' "$archive_sha256" "$archive" | sha256sum -c -
tar -xzf "$archive" -C "$build_root"

meson setup \
    "$build_root/gtk4-layer-shell-${version}/build" \
    "$build_root/gtk4-layer-shell-${version}" \
    --prefix="$prefix" \
    --libdir=lib \
    -Dexamples=false \
    -Dtests=false \
    -Ddocs=false \
    -Dintrospection=false \
    -Dvapi=false
meson compile -C "$build_root/gtk4-layer-shell-${version}/build"
meson install -C "$build_root/gtk4-layer-shell-${version}/build"

printf '%s\n' "Installed gtk4-layer-shell $version in $prefix"

