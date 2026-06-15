# Third-party notices

markdown-delight is MIT (see `LICENSE`). It builds on the following
open-source work:

## Native app (`app/`)

| Dependency | License | Role |
|---|---|---|
| [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) (+ `gpui_platform`, `gpui_wgpu`) | Apache-2.0 | GPU UI framework (consumed from a pinned zed checkout; see README §Build) |
| [comrak](https://github.com/kivikakk/comrak) | BSD-2-Clause | CommonMark + GFM parsing |
| [ropey](https://github.com/cessen/ropey) | MIT | rope text buffer (the editor core) |
| [serde](https://serde.rs) | MIT / Apache-2.0 | theme file deserialization |
| [toml](https://github.com/toml-rs/toml) | MIT / Apache-2.0 | theme file format |

The CRT chrome (`crt.rs`, `warp.rs`, workspace layout) is ported from
[terminal-delight](https://github.com/parker-brown-family/terminal-delight)
(MIT, same author). The barrel-warp render pass is a small Apache-2.0-
compatible patch (`td-crt-pass`, in `patches/td-crt-pass.patch`) applied to the
pinned gpui checkout — it touches only `crates/gpui_wgpu`. See `BUILDING.md`.

**Clean-room boundary:** Zed's `editor`, `markdown`, `rope`, `text`, and
`language` crates are GPL-3.0-or-later and are NOT used, linked, or copied.
The editor core here is original work on `ropey`. (See `docs/PLAN.md` §2.)

**Binary distribution (MIT-clean):** the vendored Zed/gpui graph would otherwise
pull in two GPL-3.0 leaf crates (`ztracing`, `zlog`). `scripts/prepare-gpui.sh`
applies `patches/sever-gpl-crates.patch` to cut them out of the build, so the
**shipped binary links no GPL code and is MIT-distributable** — it is released as
a self-contained AppImage. This is purely a packaging/link-time concern and is
independent of the clean-room rule above (the GPL editor/language crates were
never used regardless). `cargo deny check licenses` passes against the
MIT/Apache/BSD/ISC allowlist with no GPL exceptions.

## Fonts

| Font | License |
|---|---|
| JetBrains Mono | OFL-1.1 |
| VT323 | OFL-1.1 |
| Inter | OFL-1.1 |

Fonts are loaded from Google Fonts in the browser reference and expected
system-installed for the native app; they are not redistributed here.

## Browser design reference (`index.html`, `src/`)

Zero runtime dependencies. The theme engine is ported from the IMT PM
engine (same author).
