#!/usr/bin/env bash
# prepare-gpui.sh — set up the pinned, patched gpui checkout this app builds on.
#
# Idempotent. Clones zed-industries/zed as the sibling ../zed-upstream (override
# with $ZED_UPSTREAM_DIR) at the rev pinned in app/Cargo.toml
# ([package.metadata.markdown-delight] zed_rev), then applies BOTH patches:
#   td-crt-pass       — the per-pane CRT barrel-warp render pass (gpui_wgpu)
#   sever-gpl-crates  — drops the GPL-3.0 crates (ztracing / zlog) that the
#                       gpui -> sum_tree edge would otherwise link into the
#                       binary, so a *distributed* build stays MIT-clean
# Safe to re-run — already-applied patches are detected and skipped. Used by
# both local setup (see BUILDING.md) and CI.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"          # repo root
zed="${ZED_UPSTREAM_DIR:-$(cd "$here/.." && pwd)/zed-upstream}"  # sibling (overridable)
patch_crt="$here/patches/td-crt-pass.patch"
patch_gpl="$here/patches/sever-gpl-crates.patch"

rev="$(sed -n 's/^zed_rev *= *"\(.*\)"/\1/p' "$here/app/Cargo.toml" | head -1)"
[ -n "$rev" ] || { echo "!! could not read zed_rev from app/Cargo.toml" >&2; exit 1; }
for p in "$patch_crt" "$patch_gpl"; do
  [ -f "$p" ] || { echo "!! missing patch: $p" >&2; exit 1; }
done
echo "→ pinned zed rev: $rev"
echo "→ zed checkout:   $zed"

if [ ! -d "$zed/.git" ]; then
  echo "→ cloning zed-industries/zed into $zed"
  git init -q "$zed"
  git -C "$zed" remote add origin https://github.com/zed-industries/zed.git
  git -C "$zed" fetch -q --depth 1 origin "$rev"
  git -C "$zed" checkout -q FETCH_HEAD
elif [ "$(git -C "$zed" rev-parse HEAD 2>/dev/null || true)" != "$rev" ]; then
  echo "→ fetching + checking out $rev"
  git -C "$zed" fetch --depth 1 origin "$rev" 2>/dev/null || git -C "$zed" fetch origin
  git -C "$zed" checkout -q "$rev"
fi

cd "$zed"

# Apply a patch idempotently. $1=patch file, $2=label, $3=sentinel command that
# returns 0 when the change is already present (covers checkouts carrying the
# patch as commits rather than a working-tree diff).
apply_patch() {
  local pf="$1" label="$2" sentinel="$3"
  if git apply --reverse --check "$pf" 2>/dev/null; then
    echo "✓ $label already applied"
  elif git apply --check "$pf" 2>/dev/null; then
    git apply "$pf"
    echo "✓ $label applied"
  elif eval "$sentinel" 2>/dev/null; then
    echo "✓ $label present (carried as commits)"
  else
    echo "!! $label does not apply cleanly to $zed." >&2
    echo "   Regenerate $pf or reset the checkout." >&2
    exit 1
  fi
}

apply_patch "$patch_crt" "td-crt-pass" \
  'git grep -q "set_crt_rects" -- crates/gpui_wgpu/src'
# sentinel: ztracing gone from sum_tree means the sever is already in the tree
apply_patch "$patch_gpl" "sever-gpl-crates" \
  '! git grep -q "ztracing" -- crates/sum_tree'

echo "✓ gpui ready at $zed"
git status --short || true
