#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/chathead-package-links.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
found=false

for artifact in "$project_dir"/dist/*.AppImage; do
    [ -f "$artifact" ] || continue
    found=true
    appimage_dir="$work_dir/appimage"
    mkdir -p "$appimage_dir"
    (
        cd "$appimage_dir"
        "$artifact" --appimage-extract >/dev/null
    )
    "$project_dir/scripts/check-sidecar-links.sh" \
        "$appimage_dir/squashfs-root/resources/sidecar/chathead-linux" \
        "$appimage_dir/squashfs-root/resources/native/lib"
    test -f "$appimage_dir/squashfs-root/resources/gnome-extension/chathead-ai@io.github.chathead-ai/metadata.json"
    test -f "$appimage_dir/squashfs-root/resources/gnome-extension/chathead-ai@io.github.chathead-ai/extension.js"
done

for artifact in "$project_dir"/dist/*.deb; do
    [ -f "$artifact" ] || continue
    found=true
    deb_dir="$work_dir/deb"
    mkdir -p "$deb_dir"
    dpkg-deb -x "$artifact" "$deb_dir"
    sidecar=$(find "$deb_dir" -path '*/resources/sidecar/chathead-linux' -type f -print -quit)
    if [ -z "$sidecar" ]; then
        printf '%s\n' "Packaged Debian sidecar is missing" >&2
        exit 1
    fi
    native_lib=$(find "$deb_dir" -path '*/resources/native/lib' -type d -print -quit)
    if [ -z "$native_lib" ]; then
        printf '%s\n' "Packaged Debian native library directory is missing" >&2
        exit 1
    fi
    "$project_dir/scripts/check-sidecar-links.sh" "$sidecar" "$native_lib"
    extension_dir=$(find "$deb_dir" -path '*/resources/gnome-extension/chathead-ai@io.github.chathead-ai' -type d -print -quit)
    if [ -z "$extension_dir" ] || [ ! -f "$extension_dir/metadata.json" ] || [ ! -f "$extension_dir/extension.js" ]; then
        printf '%s\n' "Packaged Debian GNOME extension is missing" >&2
        exit 1
    fi
done

if [ "$found" = false ]; then
    printf '%s\n' "No AppImage or Debian artifacts were found under dist" >&2
    exit 1
fi

printf '%s\n' "Packaged sidecar link checks passed"
