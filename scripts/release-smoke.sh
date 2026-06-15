#!/usr/bin/env bash
# release-smoke.sh — run the same bar CI does, locally, before a release/push.
# Assumes ../zed-upstream is already prepared (run scripts/prepare-gpui.sh first).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here/app"

echo "▸ fmt";    cargo fmt -- --check
echo "▸ check";  cargo check --locked
echo "▸ clippy"; cargo clippy --locked -- -D warnings
echo "▸ test";   cargo test --locked
echo "▸ build";  cargo build --release --locked
echo "▸ deny";   cargo deny check
echo "▸ audit";  command -v cargo-audit >/dev/null && cargo audit || echo "  (cargo-audit not installed — skipping; CI runs it)"

# AppImage: build + structurally verify the distributable artifact.
echo "▸ appimage"
bash "$here/scripts/build-appimage.sh" >/tmp/md-appimage-build.log 2>&1 \
  || { echo "  build-appimage failed — see /tmp/md-appimage-build.log"; tail -20 /tmp/md-appimage-build.log; exit 1; }
img="$here/dist/markdown-delight-x86_64.AppImage"
[ -x "$img" ] || { echo "  missing $img"; exit 1; }
ext="$(mktemp -d)"; ( cd "$ext" && "$img" --appimage-extract >/dev/null )
sq="$ext/squashfs-root"
[ -x "$sq/usr/bin/markdown-delight" ] || { echo "  bundle missing the binary"; exit 1; }
[ -s "$sq/usr/share/licenses/markdown-delight/THIRD-PARTY-LICENSES.txt" ] || { echo "  bundle missing THIRD-PARTY-LICENSES"; exit 1; }
floor="$(objdump -T "$sq/usr/bin/markdown-delight" | grep -oE 'GLIBC_[0-9.]+' | sort -V | tail -1)"
echo "  ✓ AppImage OK (binary + licenses present; glibc floor $floor)"
rm -rf "$ext"
echo "✓ all green"
