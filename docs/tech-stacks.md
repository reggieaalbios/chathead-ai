# Tech Stack

This inventory describes the code active in the current repository. Experimental and historical components are labeled explicitly.

## Desktop application

| Technology | Where | Purpose |
| --- | --- | --- |
| Electron 43 | `src/main/` | Owns the normal settings window, tray, process lifecycle, and native sidecar process. |
| React 19 + TypeScript 5.9 | `src/renderer/` | Implements provider setup, general settings, and Local Voice settings. |
| electron-vite + Vite | `electron.vite.config.ts` | Builds and serves Electron main, preload, and renderer bundles. |
| Zustand | `src/renderer/src/store.ts` | Keeps renderer-only state such as the selected view and appearance. |
| Zod | shared protocol and renderer | Validates renderer input and selected IPC envelopes. |
| Tailwind tooling | package manifests | Available to the renderer build; the current UI is primarily authored in `styles.css`. |
| pnpm 11 | `package.json`, lockfile | Required Node package manager and script runner. |

The renderer is sandboxed, uses context isolation, has no Node integration, and receives only the allowlisted preload API. Electron never captures microphone audio or loads voice models.

## Native Rust sidecar and overlay

| Technology | Where | Purpose |
| --- | --- | --- |
| Rust 2024, MSRV 1.92 | workspace `Cargo.toml` | Implements credentials, provider state, IPC, local voice, and the native overlay. |
| `chathead-core` | `crates/chathead-core` | UI-independent provider, conversation, and versioned protocol domain. |
| `chathead-linux` | `crates/chathead-linux` | Newline-delimited IPC sidecar and GTK/Wayland overlay. |
| GTK 4 + Cairo | `overlay.rs` | Native chat panel, composer, animated orb, input controllers, and rendering. |
| `gtk4-layer-shell` | `chathead-linux` | Creates a click-through `wlr-layer-shell` overlay on supported Wayland compositors. |
| ashpd GlobalShortcuts | `overlay.rs` | Registers `Super+E` for voice and `Super+W` for panel visibility through the XDG portal, consuming Activated and Deactivated events. |
| Tokio + `futures-util` | shortcut thread | Runs portal registration and both event streams away from GTK. |
| Linux Secret Service via `keyring` | `chathead-core` | Stores provider API keys without putting secrets in IPC or plaintext configuration. |

The overlay targets Linux x86_64 and Wayland compositors with `wlr-layer-shell`, initially Hyprland. GNOME can run Settings but does not support this overlay surface. There is no XWayland or compositor-polling fallback.

## Local Voice

| Technology | Where | Purpose |
| --- | --- | --- |
| `chathead-voice` | `crates/chathead-voice` | Keeps capture, DSP, model management, VAD, inference, and lifecycle logic out of GTK. |
| CPAL 0.18 | `capture.rs` | Enumerates input-only devices and captures through PipeWire, then PulseAudio, then ALSA. Monitor and loopback sources are excluded. |
| Fixed SPSC sample buffer | `capture.rs` | Lets the realtime callback convert/copy mono samples using preallocated atomics only; it does not allocate, block, touch GTK, access files, or infer. |
| sherpa-onnx 1.13.4 Rust API | `recognizer.rs` | Runs CPU-only Whisper Tiny or Qwen3-ASR through their native offline model configs. Only the active recognizer is loaded; inference leaves at least one CPU core to the system and GTK. |
| Whisper Tiny multilingual INT8 | `model.rs` | Recognizes English, Filipino/Tagalog, and common code-switching with automatic language detection. Required installed files total about 104 MB. Whisper and Silero VAD use the MIT license; sherpa-onnx uses Apache-2.0. |
| Qwen3-ASR 0.6B INT8 | `model.rs` | Official sherpa conversion with English and Filipino language support. The model and runtime are Apache-2.0. Accuracy/default badges remain benchmark-gated. |
| rubato 4.0 | `recognizer.rs` | Performs band-limited FFT resampling on the transcription worker, outside the real-time capture callback. |
| Silero VAD | `recognizer.rs` | Rejects no-speech recordings and trims only outer silence with 250 ms context, preserving pauses inside the utterance. |
| SHA-256 + atomic install | `model.rs` | Stores each model separately, writes `.part`, validates exact archive and per-file size/hash, extracts allowlisted files including tokenizer paths, and renames only a complete model directory. |
| XDG config/data directories | `config.rs` | Stores preferences under `$XDG_CONFIG_HOME/chathead-ai` and models under `$XDG_DATA_HOME/chathead-ai/models`. |

Voice is disabled by default. Raw audio is held only in bounded memory, never written to disk, never included in IPC, and never uploaded. V1 captures live audio and transcribes after release; it does not show partial word-by-word recognition.

## Build, integrity, and packaging

| Technology | Where | Purpose |
| --- | --- | --- |
| `pnpm bootstrap` | `scripts/bootstrap-native.sh` | Builds local `gtk4-layer-shell` when needed and downloads the pinned sherpa-onnx static archive after checking its committed SHA-256. |
| `SHERPA_ONNX_ARCHIVE_DIR` | `scripts/cargo-native.sh` | Forces sherpa's build to use the verified archive. Cargo fails closed when it is missing. |
| Cargo wrapper | `scripts/cargo-native.sh` | Supplies local native build/link paths. Use this instead of plain Cargo for project checks and builds. |
| electron-builder | `package.json` | Produces Linux x86_64 AppImage and Debian packages. Debian declares the linked ALSA and PipeWire runtime dependencies; CPAL's PulseAudio backend speaks the server protocol without linking `libpulse`. |
| `ldd` packaging gate | `scripts/check-sidecar-links.sh` | Rejects a release sidecar with unresolved links or missing CPAL audio backends. |
| Vitest, rustfmt, Clippy, Rust tests | `pnpm check` | Verifies TypeScript, IPC-facing UI behavior, Rust state/DSP/integrity behavior, formatting, and warning-free code. |

## Provider transport status

ChatGPT subscription authentication is delegated to the installed Codex CLI and ChatHead never reads its token files. The embedded text conversation currently uses `codex app-server --stdio`; that command is explicitly experimental and is not presented as a stable provider transport. Local Voice does not change that status.

## Separate repository subproject

`mcp-server/rust-docs-mcp-server/` is a separate Rust MCP documentation server with its own Tokio, rmcp, OpenAI, embedding, and document-processing dependencies. It is not part of the desktop or voice runtime.

## Legacy or inactive routes

AGS v1/GTK3 JavaScript experiments and Hyprland cursor polling are historical prototype approaches only. PortAudio, archived Whisper wrappers, Electron microphone capture, speaker/system-audio capture, remote transcription, recording history, GPU inference, ARM64 packages, and continuous dictation are not implemented.
