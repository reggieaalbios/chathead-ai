# Tech Stack

This document reflects the stack currently present in the repository. If a
technology is prototype-only or not part of the active runtime, it is called out
explicitly.

## Product Runtime

| Technology | Where | What it is for |
| --- | --- | --- |
| Rust | `Cargo.toml`, `src/main.rs` | Main application language. The overlay is implemented as a native Linux process instead of a web or Electron-style shell. |
| Rust edition 2024 | `Cargo.toml` | Enables the current Rust edition for the app crate. |
| Rust 1.92+ | `Cargo.toml`, `README.md` | Minimum Rust toolchain expected by this project. |
| GTK 4 | `Cargo.toml`, `src/main.rs`, `src/style.css` | Native UI toolkit for the chathead, panel, labels, input field, event controllers, CSS styling, and drawing surface. |
| `icondata` (Lucide feature) | `Cargo.toml`, `src/icons.rs` | Compile-time SVG glyph data for simple controls and navigation icons; keeps UI symbols consistent without hand-drawn text glyphs. |
| Bundled provider artwork | `src/assets/provider/`, `src/icons.rs` | Local transparent provider marks. Brand artwork remains asset-based so it can be replaced without changing UI code. |
| Cairo via GTK | `src/main.rs` | Immediate-mode drawing for the animated companion orb, glow, face plate, and input regions. |
| Wayland layer-shell | `README.md`, `src/main.rs` | Positions the app as an overlay surface instead of a normal desktop window. This is what makes the chathead visually windowless and persistent across workspace switches. |
| `gtk4-layer-shell` Rust crate | `Cargo.toml`, `src/main.rs` | Rust binding used to configure layer-shell behavior: overlay layer, anchors, monitor targeting, keyboard mode, namespace, and exclusive zone. |
| `gtk4-layer-shell` native library | `README.md`, `scripts/bootstrap-native.sh` | System/native dependency required by the Rust crate. The bootstrap script can build version `1.3.0` into `.local-native/` when a distro package is unavailable. |
| XDG Desktop Portal GlobalShortcuts | `README.md`, `src/main.rs` | Registers the voice toggle shortcut through the desktop portal instead of compositor-specific polling. Current shortcut preference is `Super+Shift+V`. |
| `ashpd` | `Cargo.toml`, `src/main.rs` | Rust client for the desktop portal APIs, currently used for GlobalShortcuts session creation, binding, and activation events. |
| Tokio | `Cargo.toml`, `src/main.rs` | Async runtime used on a dedicated shortcut service thread for portal registration and event listening. |
| `futures-util` | `Cargo.toml`, `src/main.rs` | Stream utilities used to receive and process shortcut activation events from the portal. |
| Rust standard library `mpsc` and thread APIs | `src/main.rs` | Bridges the background shortcut service into the GTK main loop without making GTK state cross-thread. |

## Desktop and Compositor Targets

| Technology | What it is for |
| --- | --- |
| Linux | Primary operating system target for the current app. |
| Wayland | Display protocol target. The app intentionally avoids XWayland fallback. |
| `wlr-layer-shell` compositors | Required compositor capability for the overlay surface. |
| Hyprland | Initial target compositor and practical development target. |
| GNOME | Not currently supported because GNOME does not implement `wlr-layer-shell`. |
| `xdg-desktop-portal` | Desktop service layer required for the global voice shortcut flow. |

## Input, Windowing, and UI Model

| Technology or pattern | What it is for |
| --- | --- |
| Single GTK process | Keeps one persistent overlay owner for the `io.github.chathead_ai.ChatHead` application ID. Re-launching activates the existing process. |
| Full-output layer-shell surface | Gives the chathead stable surface-local pointer events across a monitor-sized area. |
| Cairo input regions | Makes transparent desktop areas click-through while keeping the chathead and open panel interactive. During drag, the input region expands so pointer motion and release stay captured. |
| `gtk::GestureDrag` | Handles click-versus-drag behavior for the chathead. |
| `gtk::EventControllerKey` | Handles local keyboard input inside the panel, currently `Esc` to stop listening mode. |
| GTK CSS | Styles the overlay window, chathead, panel, status states, message area, and prompt input from `src/style.css`. |

## Build and Developer Tooling

| Technology | Where | What it is for |
| --- | --- | --- |
| Cargo | `Cargo.toml`, `Cargo.lock`, scripts | Builds, checks, and locks the Rust application dependencies. |
| pnpm | `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml` | Script runner for project commands: `pnpm bootstrap`, `pnpm start`, `pnpm check`, and `pnpm build`. The current root package has no active JavaScript dependencies. |
| `scripts/cargo-native.sh` | `scripts/` | Wraps Cargo so local `.local-native` native libraries are available through `PKG_CONFIG_PATH` and `LD_LIBRARY_PATH` when needed. |
| `scripts/bootstrap-native.sh` | `scripts/` | Downloads, checksum-verifies, builds, and installs `gtk4-layer-shell` locally for development machines missing the native library. |
| `pkg-config` | scripts, README | Detects GTK and `gtk4-layer-shell` native libraries during development and builds. |
| Meson and Ninja | `scripts/bootstrap-native.sh` | Build system used only for local `gtk4-layer-shell` native-library bootstrap. |
| `curl`, `tar`, `sha256sum` | `scripts/bootstrap-native.sh` | Fetch and verify the native bootstrap archive. |
| Thin LTO and stripping | `Cargo.toml` release profile | Reduces release binary size while keeping release builds native. |

## Reference and Prototype Code

| Technology | Where | Status |
| --- | --- | --- |
| AGS v1 | `config.js`, `test-*.js`, `README.md` | Prototype reference only. AGS v1 is superseded and is not the active runtime. Do not treat these files as the production stack. |
| Hyprland IPC via AGS scripts | `config.js`, `test-*.js` | Historical/prototype approach for cursor and monitor experiments. The active Rust app avoids global cursor-coordinate polling. |
| GTK 3 JavaScript test snippet | `test-gesture.js` | Prototype experiment only, not part of the active GTK 4 Rust runtime. |

## Included Tooling Subproject

The repository also contains `mcp-server/rust-docs-mcp-server/`, a separate Rust
MCP server project. It is not the chathead overlay runtime, but it is present in
the repo and has its own stack:

| Technology | What it is for |
| --- | --- |
| Rust edition 2024 | Language edition for the MCP server crate. |
| `rmcp` | MCP server framework. |
| Tokio multi-thread runtime | Async runtime for the MCP server. |
| `async-openai` | OpenAI API client used by the MCP server for embeddings and summaries. |
| `text-embedding-3-small` | Embedding model named in the MCP README for semantic doc search. |
| `gpt-4o-mini-2024-07-18` | Summarization model named in the MCP README. |
| `cargo` crate with vendored OpenSSL | Builds and inspects Rust crate documentation while avoiding system OpenSSL mismatches. |
| `scraper`, `walkdir`, `ndarray`, `bincode`, `tiktoken-rs` | Documentation processing, embedding storage, token handling, and cache support. |
| XDG directories or `dirs` | Platform-specific cache/data directory handling. |
| Clap | CLI argument parsing for package IDs and feature flags. |

## Not Current Stack

| Technology | Reason |
| --- | --- |
| Tauri | A Tauri skill exists in the repo, but the active app does not currently use Tauri crates, Tauri configuration, a webview frontend, or Tauri commands. |
| Electron | Not used. The app is a native GTK/Rust process. |
| React, Vite, Tailwind, Zustand | Not used by the current overlay runtime. |
| XWayland fallback | Explicitly out of scope in the README. |
| Microphone/audio capture | Not implemented yet. The voice shortcut currently toggles visual listening mode only. |
