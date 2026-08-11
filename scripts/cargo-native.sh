#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$project_dir/.local-native"
sherpa_archive_dir="$prefix/sherpa-onnx/archive"
sherpa_archive="$sherpa_archive_dir/sherpa-onnx-v1.13.4-linux-x64-static-lib.tar.bz2"
sherpa_archive_sha256="98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c"

if [ ! -f "$sherpa_archive" ] || \
    ! printf '%s  %s\n' "$sherpa_archive_sha256" "$sherpa_archive" | sha256sum -c - >/dev/null 2>&1; then
    printf '%s\n' "Verified sherpa-onnx native archive is unavailable. Run: pnpm bootstrap" >&2
    exit 1
fi

export SHERPA_ONNX_ARCHIVE_DIR="$sherpa_archive_dir"

if pkg-config --exists 'gtk4-layer-shell-0 >= 1'; then
    exec cargo "$@"
fi

if ! PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
    pkg-config --exists 'gtk4-layer-shell-0 >= 1'; then
    printf '%s\n' "gtk4-layer-shell is unavailable. Run: pnpm bootstrap" >&2
    exit 1
fi

export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec cargo "$@"
