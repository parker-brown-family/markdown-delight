# Building markdown-delight from source

> **Status:** early/WIP. The native app is verified on **Linux · X11 · NVIDIA ·
> wgpu (Vulkan)**. Wayland and AMD/Intel GPUs are expected to work (gpui supports
> them) but are **not yet verified** — see [Hardware scope](#hardware-scope).

markdown-delight's native app is built on Zed's **gpui** GPU-UI framework
(Apache-2.0), consumed from a **pinned Zed checkout** that sits *beside* this repo.
The pin carries one small, Apache-2.0-compatible render patch — `td-crt-pass`, the
per-pane barrel-warp post pass — that lives in this repo as
[`patches/td-crt-pass.patch`](patches/td-crt-pass.patch) and is applied to that
checkout. Everything else is plain `cargo`.

This is the **only** unusual step. Once the patched `../zed-upstream` exists,
`cargo run` works like any Rust project.

## The directory layout you're aiming for

`app/Cargo.toml` references gpui via `../../zed-upstream`, so the two repos must be
**siblings** with these exact names:

```
some-parent/
├── markdown-delight/      ← this repo
└── zed-upstream/          ← a checkout of zed-industries/zed at the pin below
```

## Prerequisites

- **Rust** (stable) — <https://rustup.rs>
- **A Linux desktop with a Vulkan-capable GPU.** gpui renders through wgpu/Vulkan.
  The system libraries needed are the **same as building Zed from source** — if in
  doubt, follow Zed's Linux dependency list:
  <https://github.com/zed-industries/zed/blob/main/docs/src/development/linux.md>
  (Vulkan loader + drivers, X11/Wayland dev headers, `libxkbcommon`, `fontconfig`,
  etc.)
- **git**
- *(optional)* **imagemagick** (`convert`) — only for installing the app icon when
  you run `scripts/make-default.sh`.

## Steps

From inside this repo (`markdown-delight/`):

```bash
# 1. Clone the pinned gpui source as a SIBLING named zed-upstream.
git clone https://github.com/zed-industries/zed ../zed-upstream

# 2. Check out the exact pin this app was built against.
cd ../zed-upstream
git checkout abbe85a3321bf6cb7f5b241e623d9c2e16c29187

# 3. Apply the td-crt-pass render patch (barrel warp + per-pane CRT rects).
#    Paths below assume the sibling layout above with default directory names.
git apply ../markdown-delight/patches/td-crt-pass.patch

# 4. Build & run the app.
cd ../markdown-delight/app
cargo run -- ../README.md        # opens this README: tabs · splits · edit-by-default · CRT
cargo build --release            # optimized binary at app/target/release/markdown-delight
```

### Verify the patch applied

```bash
cd ../zed-upstream
git status --short
# expect:
#  M crates/gpui_wgpu/src/gpui_wgpu.rs
#  M crates/gpui_wgpu/src/wgpu_renderer.rs
# ?? crates/gpui_wgpu/src/crt_pass.wgsl
```

If `git apply` reports a conflict, you likely checked out the wrong commit — the
patch is generated against **exactly** `abbe85a3…`. Re-run step 2.

## Become the system default Markdown editor (optional)

```bash
cd markdown-delight
scripts/make-default.sh          # installs a .desktop entry + icon, claims text/markdown
```

Prefers the release binary; falls back to the debug build so it works right after
`cargo build`. Re-run after a release build to upgrade the registration. Revert
later with `xdg-mime default <other>.desktop text/markdown text/x-markdown`.

## Keys

`ctrl+e` source↔preview · `ctrl+s` save · `ctrl+alt+r`/`ctrl+alt+d` split
right/down · `ctrl+shift+t` new tab · `ctrl+pgup`/`ctrl+pgdn` switch tab ·
`alt+arrows` pane focus · `ctrl+w` close pane · `ctrl+p` fuzzy file finder ·
right-click a tab to rename. Click places the cursor. The live theme file is
`~/.config/markdown-delight/theme.toml` (hot-reloads while running).

## Hardware scope

The native app is **verified** only on **X11 · NVIDIA · wgpu (Vulkan)** so far.
`gpui_platform` is built with both `x11` and `wayland` features, and gpui itself
runs on AMD/Intel, so other configurations are *expected* to work — but they are
unverified, and the animated CRT effect layers are GPU-intensive. If it doesn't
start or renders wrong on your box, that's a known gap, not a surprise; please open
an issue with your GPU/driver/compositor.

## Why a patch instead of a normal dependency?

The only mature GPUI-native editor is Zed's `editor` crate, which is **GPL-3.0
(study-only)** — incompatible with this project's MIT license. So gpui is consumed
at the framework layer only (Apache-2.0), and the editor core is original work on
`ropey` + `comrak`. The `td-crt-pass` patch touches **only** `crates/gpui_wgpu`
(the wgpu renderer) to add the optional barrel-warp post pass; it adds no GPL code.
See [`docs/PLAN.md`](docs/PLAN.md) §2 for the clean-room boundary and
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) for licenses.
