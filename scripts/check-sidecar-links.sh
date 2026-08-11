#!/bin/sh
set -eu

binary=${1:-target/release/chathead-linux}
library_dir=${2:-}
if [ ! -x "$binary" ]; then
    printf '%s\n' "Sidecar binary is missing or not executable: $binary" >&2
    exit 1
fi

if [ -n "$library_dir" ]; then
    links=$(LD_LIBRARY_PATH="$library_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ldd "$binary")
else
    links=$(ldd "$binary")
fi
printf '%s\n' "$links"
if printf '%s\n' "$links" | grep -q 'not found'; then
    printf '%s\n' "Sidecar has unresolved runtime libraries" >&2
    exit 1
fi

for library in libasound libpipewire-0.3; do
    if ! printf '%s\n' "$links" | grep -q "$library"; then
        printf '%s\n' "Expected CPAL runtime dependency is absent: $library" >&2
        exit 1
    fi
done

printf '%s\n' "Sidecar audio runtime links are resolved"
