#!/usr/bin/env bash
# Build a distributable, MIT-clean markdown-delight AppImage.
#
# Why an AppImage is even possible: patches/sever-gpl-crates.patch removes the GPL
# crates the Zed graph would otherwise link (gpui -> sum_tree), so the binary is
# permissive-licensed and redistributable (see THIRD-PARTY-LICENSES inside the
# bundle). This script bundles the binary + .desktop + icon + a generated
# THIRD-PARTY-LICENSES text (cargo-about) into a single self-contained file.
#
# Graphics libraries (Vulkan loader, Wayland/X11/xkbcommon) are intentionally NOT
# bundled — a GPU app must load the *host's* driver stack, so we rely on the
# host's system libs (present on any desktop Linux) exactly like Alacritty/Zed do.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
arch="x86_64"
app="markdown-delight"
dist="$repo_root/dist"
build="$repo_root/.appimage-build"
appdir="$build/AppDir"

export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

echo "==> Ensuring patched gpui checkout (td-crt-pass + sever-gpl-crates)"
bash "$repo_root/scripts/prepare-gpui.sh"

echo "==> Release build"
( cd "$repo_root/app" && cargo build --release --locked )
# Resolve the binary. A local checkout may redirect the target dir to the shared
# ../zed-upstream/target (app/.cargo/config.toml, untracked); a fresh CI clone
# builds into app/target.
bin="$repo_root/app/target/release/$app"
[ -x "$bin" ] || bin="$(cd "$repo_root/.." && pwd)/zed-upstream/target/release/$app"
[ -x "$bin" ] || { echo "missing release binary (checked app/target and shared zed-upstream/target)" >&2; exit 1; }
echo "   binary: $bin"

echo "==> Generating THIRD-PARTY-LICENSES (cargo-about)"
if ! command -v cargo-about >/dev/null; then
    echo "   cargo-about not found; installing..."
    cargo install cargo-about --version "^0.6"
fi
tpl="$build/THIRD-PARTY-LICENSES.txt"
mkdir -p "$build"
( cd "$repo_root/app" && cargo about generate about.hbs ) > "$tpl"
[ -s "$tpl" ] || { echo "license bundle came out empty" >&2; exit 1; }

echo "==> Staging AppDir"
rm -rf "$appdir"
install -Dm755 "$bin"                              "$appdir/usr/bin/$app"
install -Dm644 "$repo_root/packaging/$app.desktop" "$appdir/usr/share/applications/$app.desktop"
install -Dm644 "$repo_root/packaging/$app.desktop" "$appdir/$app.desktop"
install -Dm644 "$repo_root/packaging/$app.svg"     "$appdir/usr/share/icons/hicolor/scalable/apps/$app.svg"
install -Dm644 "$repo_root/packaging/$app.svg"     "$appdir/$app.svg"
install -Dm644 "$tpl"                              "$appdir/usr/share/licenses/$app/THIRD-PARTY-LICENSES.txt"
install -Dm644 "$repo_root/LICENSE"               "$appdir/usr/share/licenses/$app/LICENSE"

# markdown-delight bundles its themes via include_str! and loads no runtime asset
# files, so there is nothing else to stage — just the binary, desktop, icon and
# license texts.
cat > "$appdir/AppRun" <<'APPRUN'
#!/usr/bin/env bash
HERE="$(dirname "$(readlink -f "${0}")")"
# Warm the GPU shader disk cache across runs (same as the .desktop launcher).
export __GL_SHADER_DISK_CACHE="${__GL_SHADER_DISK_CACHE:-1}"
exec "$HERE/usr/bin/markdown-delight" "$@"
APPRUN
chmod +x "$appdir/AppRun"

echo "==> Fetching appimagetool (cached)"
tool="$build/appimagetool-$arch.AppImage"
if [ ! -x "$tool" ]; then
    url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
    curl -fL --retry 3 -o "$tool" "$url"
    chmod +x "$tool"
fi

echo "==> Packing AppImage"
mkdir -p "$dist"
out="$dist/$app-$arch.AppImage"
# --appimage-extract-and-run avoids a hard FUSE dependency in CI/containers.
ARCH="$arch" "$tool" --appimage-extract-and-run "$appdir" "$out"

echo "==> Done: $out"
ls -lh "$out"
