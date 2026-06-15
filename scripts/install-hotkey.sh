#!/usr/bin/env bash
# Bind Ctrl+Alt+M to "new markdown-delight scratch pad" on GNOME.
#
# A GUI app can't grab a global hotkey itself, so we register it at the desktop
# level as a GNOME custom keybinding that runs `markdown-delight --scratch`.
# Because the app is single-instance, the keypress FORWARDS to the already-running
# primary, which pops a fresh blank scratch window instantly (no dGPU cold start).
# A GNOME global shortcut fires even while markdown-delight is focused, so this
# one binding covers "from anywhere" and "from inside the app".
#
# Ctrl+Alt+M is not a built-in GNOME media key, so nothing needs to be freed.
# Reversible:  scripts/install-hotkey.sh --uninstall
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MEDIA=org.gnome.settings-daemon.plugins.media-keys
KEYPATH=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/markdown-delight-scratch/
SCHEMA="${MEDIA}.custom-keybinding:${KEYPATH}"
ACCEL='<Primary><Alt>m'

command -v gsettings >/dev/null || { echo "gsettings not found — this installer targets GNOME."; exit 1; }
case "${XDG_CURRENT_DESKTOP:-}" in
  *GNOME*) : ;;
  *) echo "Warning: XDG_CURRENT_DESKTOP='${XDG_CURRENT_DESKTOP:-?}' is not GNOME; continuing anyway." ;;
esac

# Add/remove our relocatable path in the custom-keybindings list, preserving others.
list_add() {
  local cur; cur=$(gsettings get "$MEDIA" custom-keybindings)
  case "$cur" in *"$KEYPATH"*) return 0 ;; esac          # already present
  if [[ "$cur" == "@as []" || "$cur" == "[]" ]]; then
    gsettings set "$MEDIA" custom-keybindings "['$KEYPATH']"
  else
    gsettings set "$MEDIA" custom-keybindings "${cur%]}, '$KEYPATH']"
  fi
}
list_del() {
  local cur; cur=$(gsettings get "$MEDIA" custom-keybindings)
  cur=${cur//\'$KEYPATH\', /}
  cur=${cur//, \'$KEYPATH\'/}
  cur=${cur//\'$KEYPATH\'/}
  gsettings set "$MEDIA" custom-keybindings "$cur"
}

if [[ "${1:-}" == "--uninstall" ]]; then
  list_del || true
  echo "Removed the Ctrl+Alt+M scratch-pad binding."
  exit 0
fi

# Resolve the binary (same logic as make-default.sh): cargo's real target dir,
# release preferred, debug fallback — absolute, since gnome-settings-daemon runs
# with a minimal PATH.
BIN="$(command -v markdown-delight || true)"
if [[ -z "$BIN" ]]; then
  TARGET_DIR="$(cd "$REPO/app" && cargo metadata --no-deps --format-version 1 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
  if [[ -x "${TARGET_DIR}/release/markdown-delight" ]]; then
    BIN="${TARGET_DIR}/release/markdown-delight"
  elif [[ -x "${TARGET_DIR}/debug/markdown-delight" ]]; then
    BIN="${TARGET_DIR}/debug/markdown-delight"
    echo "!! release binary not built yet — binding the DEBUG binary for now."
    echo "   re-run after:  (cd app && cargo build --release)"
  fi
fi
[[ -n "$BIN" && -x "$BIN" ]] || { echo "no markdown-delight binary found — build one first: (cd app && cargo build --release)"; exit 1; }

list_add
gsettings set "$SCHEMA" name 'Markdown Delight — New Scratch'
gsettings set "$SCHEMA" command "${BIN} --scratch"
gsettings set "$SCHEMA" binding "$ACCEL"

echo "Bound ${ACCEL}  ->  ${BIN} --scratch"
echo "Press Ctrl+Alt+M for a fresh scratch pad (instant while the app is running)."
echo "Undo with: scripts/install-hotkey.sh --uninstall"
