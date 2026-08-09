#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
prefix="$project_dir/.local-native"

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

