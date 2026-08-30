# ChatHead AI

ChatHead AI uses an Electron/React settings application and a Rust sidecar with
two Wayland presentation backends. Electron owns the tray, setup window, and
sidecar lifecycle; Rust owns credentials, provider readiness, conversations,
voice, safe Markdown projections, links, clipboard content, and commands.

## Supported platform

- Linux x86_64
- Hyprland/wlroots through GTK4 `wlr-layer-shell`, or Ubuntu 24.04 GNOME Shell
  46 Wayland through the bundled Shell extension
- Ubuntu 24.04-compatible build environment
- GTK 4.6 or newer; XDPH GlobalShortcuts is preferred on Hyprland, with native
  Hyprland custom events as an unprivileged fallback
- Rust 1.92 or newer, Node.js, and pnpm 11

GNOME does not implement `wlr-layer-shell`. On GNOME 46, Settings offers an
explicit current-user installation of the bundled ES-module extension, which
renders Shell-native `St`/`Clutter` actors. Other GNOME versions and Xorg keep
Settings usable but disable overlay launch. There is no XWayland fallback.

## Clone the repository

Clone ChatHead together with its pinned MCP server dependency:

```sh
git clone --recurse-submodules <chathead-repository-url>
cd chathead-ai
```

If ChatHead was cloned without submodules, initialize them before installing or
building:

```sh
git submodule update --init --recursive
```

The repository pins `mcp-server/rust-docs-mcp-server` to a reviewed upstream
commit. After pulling a revision that changes that pin, run the same submodule
update command again.

## Development

Install the local layer-shell dependency when the system does not provide it and fetch the pinned, checksum-verified sherpa-onnx native archive:

```sh
pnpm bootstrap
pnpm install
pnpm dev
```

`pnpm dev` builds the debug Rust sidecar and starts electron-vite. Closing the
settings window hides it; use the tray to reopen settings, launch or stop the
overlay, or quit both processes.

Run every automated check:

```sh
pnpm check
```

This runs TypeScript, ESLint, Vitest, rustfmt, Clippy with warnings denied, and
all Rust tests.

## Production build and packaging

```sh
pnpm build
pnpm dist:linux
```

The first command builds the release sidecar and production Electron bundles.
The second creates x86_64 AppImage and Debian artifacts. The package includes
the Rust sidecar, bundled GNOME 46 extension source, and locally bootstrapped
`gtk4-layer-shell` runtime. Debian
declares ALSA and PipeWire runtime dependencies; CPAL's PulseAudio backend uses
the server protocol directly, so it does not add a `libpulse` link; packaging also
runs `ldd` against the release sidecar.

For a repository-backed local launcher:

```sh
pnpm install:local
chathead-ai
```

## Security and IPC

The sidecar speaks newline-delimited JSON protocol version 12 over stdin/stdout.
Stdout is reserved for protocol messages; diagnostics use stderr. Snapshots
contain provider status and overlay state but never credential values.

The renderer is sandboxed with context isolation and no Node integration. Its
preload exposes only the allowlisted backend and window operations. Production
uses a restrictive Content Security Policy, local assets, and blocked external
navigation. API-key fields are component-local, cleared after every completed
save attempt, and never persisted or logged by JavaScript.

ChatGPT subscription authentication delegates to the installed Codex CLI.
ChatHead does not inspect or copy Codex token files. Binary resolution checks
`CHATHEAD_CODEX_BIN`, desktop `PATH`, `~/.local/bin`, and installed NVM Node
versions.

## Experimental Codex chat

The native panel includes one explicitly experimental, Linux-local text
conversation backed by `codex app-server --stdio` and an authenticated ChatGPT
subscription. Codex app-server is intended for rich embedded clients but is
still experimental and is not a supported production integration.

Each panel session uses an ephemeral Codex thread, the installed default model,
an empty temporary working directory, read-only/no-network turn sandboxing, and
no approval UI or tool controls. ChatHead renders only assistant text deltas;
it does not expose reasoning, command, file-change, account, token, thread, or
credential data to Electron snapshots or logs. Closing the panel preserves the
in-memory transcript, while stopping ChatHead or quitting terminates Codex and
clears the conversation.

## Native overlay behavior

The overlay remains a monitor-sized GTK layer surface. Its compositor input
region contains only the orb and open panel at rest, expands for dragging, and
returns to click-through behavior after release. The panel is kept within the
output edges.

On GNOME 46 Wayland, the extension owns only the Shell actors for the orb and
panel. The sidecar exposes independent presentation protocol version 1 over the
session bus; snapshots contain revisioned, validated UI data and no credentials,
tokens, raw audio, model files, or Codex identifiers. Revision gaps cause a full
resync. Losing the extension bus owner stops the logical overlay and launch is
disabled until integration is available again.

GNOME support remains acceptance-gated: package validation is automated, while
fullscreen stacking, Overview/lock hiding, mixed scaling, focus, drag, and
multi-monitor behavior must pass the Ubuntu 24.04 GNOME 46 live matrix before a
release is advertised as fully supported.

Local Voice is off by default. Settings offers independently downloadable,
checksum-verified Whisper Tiny multilingual INT8 and Qwen3-ASR 0.6B INT8
models. Whisper remains the fresh-install selection until the documented
English/Filipino/code-switching accuracy and hardware performance gates have
been completed; Qwen can already be downloaded and activated explicitly.
Voice input has no global shortcut on a fresh install. Configure one under
Keyboard Shortcuts; Hold mode captures on press and transcribes on release,
while Toggle mode changes state on activation and ignores release. English,
Filipino/Tagalog, and common code-switching are recognized locally. The final
transcript enters the existing composer and sends after a fixed 0.7-second Esc
cancellation window.

Panel visibility likewise has no reserved shortcut until the user configures
one. A configured panel shortcut can launch the overlay from the tray process
when provider and desktop readiness pass; otherwise Settings shows the exact
readiness blocker. The companion orb remains available while the panel is hidden.

On an actively verified Hyprland session, ChatHead stores its approved shortcut
state in `$XDG_CONFIG_HOME/chathead-ai/shortcuts.json` and generates one managed
fragment under `$XDG_CONFIG_HOME/hypr/chathead-ai`. Hyprland 0.55+ uses Lua;
0.54 and earlier use an isolated legacy hyprlang adapter. Existing bindings are
only shadowed after an explicit warning and their original configuration lines
are never edited, so clearing the ChatHead shortcut restores them naturally.
No root access or `/dev/input` access is used. Replacement cannot override
kernel, TTY, secure-attention, or hardware-reserved sequences, and ChatHead must
be running for its actions to activate.

Capture uses the native Rust sidecar through PipeWire, PulseAudio, or ALSA. Raw
audio stays in bounded memory, is never saved, is never sent through Electron
IPC, and is never uploaded. Recording auto-finalizes after 30 seconds.
No-speech recordings, device loss, overflow, empty transcripts, and busy-chat
attempts are discarded without sending a prompt.

## Experimental native Agent mode

The native GTK panel can switch a session from safe Chat to experimental Agent
mode. Agent requires a folder selected through the XDG desktop portal and runs
Codex App Server turns in that folder with `approvalPolicy: "never"` and
`dangerFullAccess`. It can read, edit, delete, run commands, and use the network
with the launching user's privileges. Cached or passwordless `sudo` can allow
elevation without another prompt.

The full-access acknowledgment is the only Agent value persisted, under the
ChatHead XDG configuration directory. Conversation text, activity details,
questions, selected folders, and Codex thread ids remain in memory for the
running session. New Chat always returns to safe Chat mode. The GNOME Shell
presentation remains Chat-only in this release; Agent controls and activity are
available only in the native GTK panel. Codex App Server and interactive user
input requests are experimental, so this capability is not production-stable.

Private local comparisons use a tab-separated manifest whose rows contain an
absolute mono-WAV path and reference transcript. Audio remains outside git:

```bash
pnpm benchmark:voice -- sherpa-onnx-whisper-tiny-int8-multilingual-v1 /absolute/path/manifest.tsv
pnpm benchmark:voice -- sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25 /absolute/path/manifest.tsv
```
