# Changelog

All notable changes to markdown-delight are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it
reaches 1.0. Until then, `0.x` minor bumps may include breaking changes.

## [Unreleased]

### Added

- **Save As** (`Ctrl+Shift+S`) — a native "choose where to save" dialog
  (`prompt_for_new_path`); writes there and adopts the path, renaming the tab.
  `Ctrl+S` still saves in place (scratch buffers auto-name into the notebook dir).
- **Ctrl+Alt+M scratch-pad hotkey** — a system keybinding
  (`scripts/install-hotkey.sh`) that pops a fresh, blank scratch window; forwards
  to the running instance in ~4ms (no GPU cold start), with no session restore.
- **Display config** — per-pane appearance split into four independently-
  inheriting groups (colour / texture / grade / curve) with a monitor-OSD tray;
  default look is paper outer / hacker inner, remembered across sessions.
- **Comment mode** — Google-Docs-style review (block + range comments, decay,
  all-comments browser, copy-with-comments, export with a `.git/info/exclude`
  guard).
- **IDE text selection + clipboard** in the source editor.

### Project

- CI (fmt · clippy `-D warnings` · test · build · `cargo-deny`), `app/deny.toml`,
  `scripts/prepare-gpui.sh` + `release-smoke.sh`.

## [0.1.0] — 2026-06-14

First public, source-only release. A tabful, tiling Linux Markdown editor
(Rust + gpui + comrak) that renders Markdown natively — no webview — with a
hot-reloadable, CRT-flavored visual identity.

### Added

- **Native Markdown render.** comrak parses CommonMark + GFM (tables, task
  lists, strikethrough, autolinks) into GPUI elements — headings, inline runs,
  code blocks, blockquotes, lists — rendered on the GPU, no webview.
- **Live source ▸ preview.** A rope-backed (`ropey`) source editor; every
  preview pane of the same document re-renders live as you type. `Ctrl+E` flips
  a pane between source and rendered.
- **IDE-grade text editing.** Full selection model — Shift+Arrows/Home/End,
  Ctrl(+Shift) word and document motion, Ctrl+A select-all, system clipboard
  (cut/copy/paste), Ctrl+Backspace/Delete word-delete, and shift-click extend.
  Atomic save (write-temp-then-rename).
- **Comment mode — Google-Docs-style review.** A per-pane, read-only review
  surface (`Ctrl+Shift+C` or the `▣ comment` header chip). Click a block to
  comment on it, or drag-select a span inside a paragraph to comment on just
  that text. The open thread shows in a solid, non-CRT "device" panel with a
  quoted-text magnifier and physical Add/Done/Resolve buttons; commented text
  glows and carries a count badge. An all-comments browser (`Ctrl+Shift+A`)
  lists every thread with a **Deprecated** section for anchors whose text was
  edited away (re-anchored by content fingerprint + quote on every reparse —
  kept, never silently dropped). Comments persist to
  `~/.config/markdown-delight/comments/<key>.json`, **never beside the `.md`**,
  so they can't be committed by accident; an Export action writes a sidecar
  guarded locally via `.git/info/exclude`.
- **Tiling multi-pane.** True tiling-tree splits (`Ctrl+Alt+R` / `Ctrl+Alt+D`)
  that divide only the focused pane, a tab strip, `Alt+←/→` focus movement,
  sub-tab drag-to-split/move, a pop-out scratch window with tear-off, scratch
  notebooks, and session restore.
- **Per-pane themes + seed colour.** Four built-ins (`hacker`,
  `tactical-overdrive`, `field-command`, `quiet-command`) plus a seed-colour
  wheel, hot-reloaded from `~/.config/markdown-delight/theme.toml` on save.
- **Monitor-wrap CRT.** Master bezel + per-screen sub-frames, scanlines,
  rolling tracking band, flicker, jiggle, and a real per-pane barrel warp via
  the vendored `td-crt-pass` gpui renderer patch.
- **Desktop integration.** `Ctrl+P` fuzzy file finder; `scripts/make-default.sh`
  registers markdown-delight as the system default `.md` handler (`xdg-mime` +
  `.desktop` + CRT icon).
- **Single-instance forwarding.** Later launches (e.g. tray clicks) snap an open
  window forward instead of cold-starting the GPU.

### Project / packaging

- MIT-licensed own source; the editor core is clean-room original work on
  `ropey` (Zed's GPL `editor` crate was never used or copied). Binaries are
  **not** MIT-distributable because the vendored Zed/gpui graph links GPL-3.0
  crates — see [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). This is a
  **source-only** release.
- Contributor docs: [`CONTRIBUTING.md`](CONTRIBUTING.md), [`SECURITY.md`](SECURITY.md),
  [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md), and the build plan in
  [`docs/PLAN.md`](docs/PLAN.md).

### Platform

- Linux only (X11 & Wayland via gpui's wgpu renderer); verified on X11 ·
  NVIDIA · Vulkan. Not macOS/Windows.

[Unreleased]: https://github.com/parker-brown-family/markdown-delight/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/parker-brown-family/markdown-delight/releases/tag/v0.1.0
