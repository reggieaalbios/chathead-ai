#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$project_dir/.local-native"
layer_shell_version="1.3.0"
layer_shell_url="https://github.com/wmww/gtk4-layer-shell/archive/refs/tags/v${layer_shell_version}.tar.gz"
layer_shell_sha256="1ebb01ab14e98afd1727f68f64981c37bd23305b1f131f5667c02b94cf593192"
sherpa_version="1.13.4"
sherpa_archive_name="sherpa-onnx-v${sherpa_version}-linux-x64-static-lib.tar.bz2"
sherpa_archive_url="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${sherpa_version}/${sherpa_archive_name}"
sherpa_archive_sha256="98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c"
sherpa_archive_dir="$prefix/sherpa-onnx/archive"

for command_name in curl meson ninja pkg-config sha256sum tar; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf '%s\n' "Missing required build tool: $command_name" >&2
        exit 1
    fi
done

if PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
    pkg-config --exists 'gtk4-layer-shell-0 >= 1'; then
    printf '%s\n' "gtk4-layer-shell is already available"
else
    build_root=$(mktemp -d "${TMPDIR:-/tmp}/chathead-native.XXXXXX")
    trap 'rm -rf "$build_root"' EXIT HUP INT TERM
    layer_shell_archive="$build_root/gtk4-layer-shell.tar.gz"
    curl -fL "$layer_shell_url" -o "$layer_shell_archive"
    printf '%s  %s\n' "$layer_shell_sha256" "$layer_shell_archive" | sha256sum -c -
    tar -xzf "$layer_shell_archive" -C "$build_root"

    meson setup \
        "$build_root/gtk4-layer-shell-${layer_shell_version}/build" \
        "$build_root/gtk4-layer-shell-${layer_shell_version}" \
        --prefix="$prefix" \
        --libdir=lib \
        -Dexamples=false \
        -Dtests=false \
        -Ddocs=false \
        -Dintrospection=false \
        -Dvapi=false
    meson compile -C "$build_root/gtk4-layer-shell-${layer_shell_version}/build"
    meson install -C "$build_root/gtk4-layer-shell-${layer_shell_version}/build"
    printf '%s\n' "Installed gtk4-layer-shell $layer_shell_version in $prefix"
fi

mkdir -p "$sherpa_archive_dir"
sherpa_archive="$sherpa_archive_dir/$sherpa_archive_name"
if [ -f "$sherpa_archive" ] && printf '%s  %s\n' "$sherpa_archive_sha256" "$sherpa_archive" | sha256sum -c - >/dev/null 2>&1; then
    printf '%s\n' "Verified sherpa-onnx native archive $sherpa_version"
else
    sherpa_part="$sherpa_archive.part"
    curl -fL "$sherpa_archive_url" -o "$sherpa_part"
    printf '%s  %s\n' "$sherpa_archive_sha256" "$sherpa_part" | sha256sum -c -
    mv "$sherpa_part" "$sherpa_archive"
    printf '%s\n' "Installed verified sherpa-onnx native archive $sherpa_version"
fi
