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
echo "✓ all green"
