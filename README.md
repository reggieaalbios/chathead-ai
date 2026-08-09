# ChatHead AI

ChatHead AI uses an Electron/React settings application and a Rust/GTK4
layer-shell sidecar. Electron owns the tray, setup window, and sidecar
lifecycle; Rust owns credentials, provider readiness, Codex authentication,
the Wayland overlay, drag/input regions, panel, and global shortcut.

## Supported platform

- Linux x86_64
- Wayland compositor with `wlr-layer-shell` for the overlay (initially Hyprland)
- Ubuntu 22.04-compatible build environment
- GTK 4.6 or newer and an XDG portal with GlobalShortcuts support
- Rust 1.92 or newer, Node.js, and pnpm 11

GNOME does not implement `wlr-layer-shell`; settings can open there, but the
native overlay reports `LAYER_SHELL_UNSUPPORTED` rather than silently falling
back to XWayland.

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

Install the local layer-shell dependency when the system does not provide it:

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
the Rust sidecar and the locally bootstrapped `gtk4-layer-shell` runtime.

For a repository-backed local launcher:

```sh
pnpm install:local
chathead-ai
```

## Security and IPC

The sidecar speaks newline-delimited JSON protocol version 2 over stdin/stdout.
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
output edges. `Super+Shift+V` toggles the current visual listening state through
the XDG GlobalShortcuts portal; microphone capture is not part of this release.
