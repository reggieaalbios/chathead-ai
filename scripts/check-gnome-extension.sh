#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_dir="$project_dir/gnome-extension/chathead-ai@io.github.chathead-ai"
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/chathead-gnome-extension.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

command -v gnome-extensions >/dev/null 2>&1 || {
    printf '%s\n' "gnome-extensions is required to validate the bundled GNOME backend" >&2
    exit 1
}

(cd "$source_dir" && gnome-extensions pack --quiet --force --out-dir "$work_dir" \
    --extra-source chathead-orb.svg .)
archive=$(find "$work_dir" -maxdepth 1 -name '*.zip' -type f -print -quit)
[ -n "$archive" ] || {
    printf '%s\n' "GNOME extension validation did not produce an archive" >&2
    exit 1
}

unzip -Z1 "$archive" | grep -qx 'chathead-orb.svg' || {
    printf '%s\n' "GNOME extension package is missing chathead-orb.svg" >&2
    exit 1
}

printf '%s\n' "GNOME Shell extension package validation passed"
