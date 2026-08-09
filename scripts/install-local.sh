#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bin_dir="${HOME}/.local/bin"
applications_dir="${HOME}/.local/share/applications"
wrapper="${bin_dir}/chathead-ai"
desktop_file="${applications_dir}/io.github.chathead_ai.ChatHead.desktop"

pnpm --dir "$project_dir" build

mkdir -p "$bin_dir" "$applications_dir"

tmp_wrapper=$(mktemp "${TMPDIR:-/tmp}/chathead-ai-wrapper.XXXXXX")
sed \
    -e "s|@PROJECT_DIR@|$project_dir|g" \
    "$project_dir/scripts/templates/chathead-ai.in" >"$tmp_wrapper"
install -m 0755 "$tmp_wrapper" "$wrapper"
rm -f "$tmp_wrapper"

tmp_desktop=$(mktemp "${TMPDIR:-/tmp}/chathead-ai-desktop.XXXXXX")
sed \
    -e "s|@EXEC@|$wrapper|g" \
    "$project_dir/scripts/templates/io.github.chathead_ai.ChatHead.desktop.in" >"$tmp_desktop"
install -m 0644 "$tmp_desktop" "$desktop_file"
rm -f "$tmp_desktop"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
fi

printf '%s\n' "Installed terminal command: $wrapper"
printf '%s\n' "Installed desktop launcher: $desktop_file"
printf '%s\n' "Run from terminal with: chathead-ai"

case ":${PATH}:" in
    *":$bin_dir:"*) ;;
    *)
        printf '%s\n' "Note: $bin_dir is not currently in PATH for this shell."
        printf '%s\n' "Add this to your shell config if needed: export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac
