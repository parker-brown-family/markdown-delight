#!/usr/bin/env bash
# prepare-gpui.sh — set up the pinned, patched gpui checkout this app builds on.
#
# Idempotent: clones zed-industries/zed as the sibling ../zed-upstream at the
# revision pinned in app/Cargo.toml ([package.metadata.markdown-delight] zed_rev),
# then applies patches/td-crt-pass.patch (the per-pane CRT barrel-warp render
# pass). Safe to re-run — a fast path no-ops when already prepared. Used by both
# local setup (see BUILDING.md) and CI.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # repo root (markdown-delight/)
zed="$(cd "$here/.." && pwd)/zed-upstream"                # sibling checkout
patch="$here/patches/td-crt-pass.patch"

rev="$(sed -n 's/^zed_rev *= *"\(.*\)"/\1/p' "$here/app/Cargo.toml" | head -1)"
if [ -z "$rev" ]; then
  echo "!! could not read zed_rev from app/Cargo.toml" >&2
  exit 1
fi
echo "→ pinned zed rev: $rev"

# Fast path: already at the pin with the patch applied → nothing to do. (A
# successful reverse-apply check means the patch's changes are present.)
if [ -d "$zed/.git" ] \
  && [ "$(git -C "$zed" rev-parse HEAD 2>/dev/null || true)" = "$rev" ] \
  && git -C "$zed" apply --reverse --check "$patch" 2>/dev/null; then
  echo "✓ gpui already prepared at $rev"
  exit 0
fi

if [ ! -d "$zed/.git" ]; then
  echo "→ cloning zed-industries/zed into $zed"
  git clone https://github.com/zed-industries/zed "$zed"
fi

# Discard any prior patch so the checkout is clean, then pin + (re)apply.
git -C "$zed" checkout -q -- . 2>/dev/null || true
echo "→ fetching + checking out $rev"
git -C "$zed" fetch --depth 1 origin "$rev" 2>/dev/null || git -C "$zed" fetch origin
git -C "$zed" checkout -q "$rev"
echo "→ applying td-crt-pass.patch"
git -C "$zed" apply "$patch"

echo "✓ gpui ready at $zed"
git -C "$zed" status --short
