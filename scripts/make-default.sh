#!/usr/bin/env bash
# make markdown-delight the system default for Markdown files (docs/PLAN.md §2 D5).
# This is the literal "right-click a .md → opens in our app" requirement.
#
# Installs a .desktop entry with an ABSOLUTE Exec path (desktop launchers don't
# share your shell PATH) and claims the Markdown MIME types via xdg-mime.
# Prefers the release binary; falls back to the debug build so this works the
# moment `cargo check`/`cargo build` has run. Re-run after a release build to
# upgrade the registration. Packaged builds (AppImage/Flatpak, 0.2) replace this.
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP_SRC="${APP_DIR}/packaging/markdown-delight.desktop"
APPS_DIR="${HOME}/.local/share/applications"

# .cargo/config.toml redirects builds into the shared zed-upstream target dir —
# ask cargo where artifacts actually land instead of assuming app/target/.
TARGET_DIR="$(cd "${APP_DIR}/app" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

if [[ -x "${TARGET_DIR}/release/markdown-delight" ]]; then
  BIN="${TARGET_DIR}/release/markdown-delight"
elif [[ -x "${TARGET_DIR}/debug/markdown-delight" ]]; then
  BIN="${TARGET_DIR}/debug/markdown-delight"
  echo "!! release binary not built yet — registering the DEBUG binary for now."
  echo "   re-run this script after:  (cd app && cargo build --release)"
else
  echo "!! no binary found under ${TARGET_DIR}/{release,debug}/"
  echo "   build one first:  (cd app && cargo build)  or  cargo build --release"
  exit 1
fi

mkdir -p "$APPS_DIR"

# install the desktop entry with the absolute binary path baked into Exec=
sed "s|^Exec=.*|Exec=${BIN} %F|" "$DESKTOP_SRC" > "${APPS_DIR}/markdown-delight.desktop"
chmod 644 "${APPS_DIR}/markdown-delight.desktop"
update-desktop-database "$APPS_DIR" 2>/dev/null || true

# install the CRT-monitor icon at all hicolor sizes (needs imagemagick)
ICON_SVG="${APP_DIR}/packaging/markdown-delight.svg"
if command -v convert >/dev/null && [[ -f "$ICON_SVG" ]]; then
  for s in 32 48 64 128 256; do
    mkdir -p "${HOME}/.local/share/icons/hicolor/${s}x${s}/apps"
    convert -background none -resize "${s}x${s}" "$ICON_SVG" \
      "${HOME}/.local/share/icons/hicolor/${s}x${s}/apps/markdown-delight.png"
  done
  mkdir -p "${HOME}/.local/share/icons/hicolor/scalable/apps"
  cp "$ICON_SVG" "${HOME}/.local/share/icons/hicolor/scalable/apps/"
  gtk-update-icon-cache "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi

# claim the Markdown MIME types as the default handler
xdg-mime default markdown-delight.desktop text/markdown
xdg-mime default markdown-delight.desktop text/x-markdown

echo "==> markdown-delight is now the default Markdown editor (binary: ${BIN})"
echo "    verify:  xdg-mime query default text/markdown   (expect markdown-delight.desktop)"
echo "    test:    xdg-open some-file.md   — or right-click a .md in the file manager"
echo
echo "    revert later with:  xdg-mime default <other>.desktop text/markdown text/x-markdown"
