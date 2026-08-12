# GNOME 46 acceptance gate

ChatHead's initial GNOME target is Ubuntu 24.04, GNOME Shell 46, Wayland only.
The extension must not be advertised as fully supported until every live item
below passes with the packaged AppImage and Debian builds.

## Installation and recovery

- Fresh current-user install requires explicit confirmation.
- Update/repair uses `gnome-extensions pack`, `install --force`, and `enable`
  without a shell or root privileges.
- Disabled, incompatible, missing, and unavailable states remain distinct.
- Extension disable, reload, sidecar restart, and logout/login recover cleanly.
- Losing the extension bus owner immediately hides and stops the logical overlay.

## Shell behavior

- Orb and panel remain above ordinary and fullscreen windows on every workspace.
- Neither actor appears in Alt-Tab, the dock, task lists, or workspace previews.
- Overview, lock, unlock, login transition, and extension disable hide all actors.
- Only the orb and visible panel accept pointer input; the desktop stays interactive.
- Monitor removal, resolution changes, mixed scaling, drag, size, and placement clamp.
- Composer focus, keyboard navigation, text selection, and transcript scrolling work.

## Chat parity and privacy

- Streaming Markdown, tables, lists, code, copy formats, retry, and new chat match GTK.
- Unsafe links are blocked; safe links require Rust-owned confirmation before opening.
- Theme, zoom, panel size, shortcut status, and local voice states remain synchronized.
- Logs contain no transcripts, link contents, credentials, tokens, raw audio, model
  files, or Codex internal identifiers.

## Regression and unsupported sessions

- Hyprland validates the real layer surface, input region, dragging, shortcuts,
  focus, Markdown, voice, and persistence after the controller changes.
- GNOME Xorg, later untested GNOME releases, and unsupported compositors keep
  Settings usable while launch remains disabled with an accurate explanation.
