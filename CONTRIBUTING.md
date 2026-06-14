# Contributing to markdown-delight

Thanks for being here. There are two ways in, and you do **not** need to write
Rust for the first one.

1. **Themes** — author a `.toml`, no compiler required. This is the wide path.
2. **Code** — the Rust app (`app/`) and the gpui renderer patch.

First, one rule that shapes everything:

## The one hard rule: source-only, no prebuilt binaries

markdown-delight's own source is **MIT**, and the editor core is **clean-room
original work** on `ropey` — Zed's GPL `editor` crate was never used or copied.
But the pinned Zed dependency graph links **GPL-3.0-or-later** crates into the
*built binary* through `gpui`. So:

- **Source** stays cleanly MIT — those GPL crates are never redistributed in this
  tree; you build them yourself from your own Zed checkout.
- **A distributed binary** is a derivative work of the GPL crates and would have
  to ship under GPL-3.0-or-later with corresponding source.

**Do not attach prebuilt binaries to PRs, issues, or releases.** See
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) for the full story. By
contributing you agree your contribution is licensed MIT (the repo's license).

---

## Path 1 — Themes (no Rust)

Themes are plain TOML data files. The app hot-reloads them: edit the file while
it's running and the change lands in ~300ms, no restart, no recompile.

### Try it live in 30 seconds

Your config theme lives at `~/.config/markdown-delight/theme.toml` (seeded from
`hacker` on first run). Open it, change `accent`, save — watch the running app
update.

### Anatomy of a theme

Copy [`app/themes/hacker.toml`](app/themes/hacker.toml) as a starting point. The
key fields:

```toml
name = "midnight"          # registry id / identity
icon = "☾"                 # the glyph that stands in for the theme in the picker

[colors]
bg      = "#03100a"        # window background
surface = "#071a10"        # panels, headers, tray
text    = "#86efac"        # default foreground
accent  = "#22c55e"        # focus borders, cursor glow, highlights
faint   = "#14401f"        # dim chrome, inactive borders

[effects]                  # all optional; 0 = off
scanline_opacity = 0.22    # CRT scanline darkness
vignette         = 0.8     # corner falloff
glow             = 0.85    # accent glow (header, cursor)
bloom            = 0.9     # centre phosphor bloom
tracking         = 0.6     # rolling tracking-band strength
flicker          = 0.5     # stepped flicker
jiggle           = 0.7     # rare vertical-hold hop
curvature        = 0.8     # barrel warp — needs the td-crt-pass renderer patch
screen_glare     = 0.42    # top-left glass reflection
```

Notes:

- **`curvature`** only bends if you've applied the `td-crt-pass` renderer patch
  (see *Dev setup* below). Without it the dial is a no-op.
- Want it effect-free (a clean, modern look)? Set the whole `[effects]` block to
  zeros — see [`app/themes/quiet-command.toml`](app/themes/quiet-command.toml).

### Submitting a built-in theme

1. Drop `app/themes/<your-theme>.toml` in the repo.
2. Register it in `BUILTIN_THEMES` in
   [`app/src/theme.rs`](app/src/theme.rs) (one `(id, include_str!(...))` line).
   Keep `name` in the file equal to the registry id — a test enforces this.
3. Run `cargo test` (the theme-parsing tests will validate your file).
4. Add a screenshot to the PR.

A good built-in is internally coherent and legible at a glance. Loud is fine;
unreadable is not.

---

## Path 2 — Code

### Dev setup

markdown-delight consumes `gpui` from a **pinned Zed checkout** beside this repo,
carrying the `td-crt-pass` renderer patch (the per-pane barrel warp):

```bash
git clone https://github.com/zed-industries/zed ../zed-upstream
cd ../zed-upstream
git checkout abbe85a3321bf6cb7f5b241e623d9c2e16c29187
git apply ../markdown-delight/patches/td-crt-pass.patch
cd ../markdown-delight/app && cargo run -- ../README.md
```

Full prerequisites, system dependencies, and troubleshooting are in
[`BUILDING.md`](BUILDING.md).

### The bar (run it before you push)

```bash
cd app
cargo fmt -- --check
cargo clippy --locked -- -D warnings   # warnings are errors
cargo test --locked
cargo build --release --locked
```

For the browser design reference, from the repo root: `node --check src/js/*.js`.

### Clean-room rule for Zed (important)

Zed's `editor`, `markdown`, `rope`, `text`, and `language` crates are
**GPL-3.0-or-later — study only, never copy**. You may learn *architectural
facts* from Zed. You may **not** transcribe function bodies, identifiers, or
structure. The editor core here is written from the `ropey` / `comrak` APIs
(MIT / BSD-2), not with Zed source open. See [`docs/PLAN.md`](docs/PLAN.md) §2.

### PRs

- One focused change per PR; describe the *why*.
- Include test output / a screenshot for anything user-visible.
- Keep new code in the idiom of the file around it.
- See [`docs/PLAN.md`](docs/PLAN.md) for the roadmap and where things are headed.

Questions? Open an issue — happy to help you land your first theme or patch.
