#!/usr/bin/env bash
# package-windows-gnu.sh — cross-build deskunion for Windows (x86_64-pc-windows-gnu)
# and assemble a distributable zip in dist/.
#
# Requirements (Fedora):
#   - mingw64-* stack: mingw64-gcc, mingw64-gtk4, mingw64-glib2, mingw64-pango,
#     mingw64-opus, mingw64-openssl, mingw64-librsvg2, ...
#   - rustup target add x86_64-pc-windows-gnu
#   - libadwaita cross-built into MINGW_PREFIX below (Fedora has no mingw64-libadwaita).
#
# The pkg-config overrides MUST be target-scoped (PKG_CONFIG_*_x86_64_pc_windows_gnu):
# setting PKG_CONFIG globally poisons host-side build scripts (e.g. libgit2-sys via
# shadow-rs) with mingw include paths.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TARGET=x86_64-pc-windows-gnu
TARGET_DIR=target/windows-gnu-workspace
MINGW_SYSROOT=/usr/x86_64-w64-mingw32/sys-root/mingw
MINGW_PREFIX=${MINGW_PREFIX:-/tmp/deskunion-mingw-prefix}
BUNDLE_NAME=deskunion-windows-x86_64
STAGING="dist/$BUNDLE_NAME"

# --- 1. build ---------------------------------------------------------------
# NOTE: do NOT use the mingw64-pkg-config wrapper here — it sets a sysroot and
# would rewrite the -L path of the libadwaita prefix to a nonexistent location.
# The host pkg-config + target-scoped PKG_CONFIG_LIBDIR resolves the mingw .pc
# files (absolute sysroot paths) and the prefix .pc verbatim, without leaking
# mingw paths into host-side build scripts.
PKGCONFIG_DIRS="$MINGW_PREFIX/lib/pkgconfig:$MINGW_SYSROOT/lib/pkgconfig:$MINGW_SYSROOT/share/pkgconfig"
env -u PKG_CONFIG -u PKG_CONFIG_PATH -u PKG_CONFIG_LIBDIR \
    PKG_CONFIG_x86_64_pc_windows_gnu=pkg-config \
    PKG_CONFIG_LIBDIR_x86_64_pc_windows_gnu="$PKGCONFIG_DIRS" \
    PKG_CONFIG_ALLOW_CROSS_x86_64_pc_windows_gnu=1 \
    cargo build --release --workspace --target "$TARGET" --target-dir "$TARGET_DIR"

EXE="$TARGET_DIR/$TARGET/release/deskunion.exe"
[[ -f "$EXE" ]] || { echo "build did not produce $EXE" >&2; exit 1; }

# --- 2. stage ---------------------------------------------------------------
rm -rf "$STAGING" "dist/$BUNDLE_NAME.zip"
mkdir -p "$STAGING/bin" "$STAGING/share/glib-2.0/schemas" "$STAGING/share/icons"
cp "$EXE" "$STAGING/bin/"

# DLL closure: walk objdump imports, resolve case-insensitively in the mingw
# sysroot + libadwaita prefix, copy and recurse. Windows system DLLs
# (kernel32.dll, msvcrt.dll, ...) are simply not found and skipped.
declare -A DLL_INDEX=()
while IFS= read -r -d '' dll; do
    DLL_INDEX["$(basename "$dll" | tr '[:upper:]' '[:lower:]')"]="$dll"
done < <(find "$MINGW_SYSROOT/bin" "$MINGW_PREFIX/bin" -maxdepth 1 -iname '*.dll' -print0)

declare -A SEEN=()
queue=("$STAGING/bin/deskunion.exe")
while ((${#queue[@]})); do
    current="${queue[0]}"; queue=("${queue[@]:1}")
    while read -r dep; do
        key="$(tr '[:upper:]' '[:lower:]' <<< "$dep")"
        [[ -n "${SEEN[$key]:-}" ]] && continue
        SEEN[$key]=1
        [[ -n "${DLL_INDEX[$key]:-}" ]] || continue   # system DLL
        cp "${DLL_INDEX[$key]}" "$STAGING/bin/"
        queue+=("${DLL_INDEX[$key]}")
    done < <(x86_64-w64-mingw32-objdump -p "$current" | awk '/DLL Name/ {print $3}')
done

# runtime data: gsettings schemas (compiled with the host tool — both ends are
# little-endian) and the Adwaita icon theme. gdk-pixbuf external loaders are
# skipped on purpose: png/jpeg are built into gdk-pixbuf 2.44 and GTK 4.21
# renders SVG icons natively.
cp "$MINGW_SYSROOT"/share/glib-2.0/schemas/*.xml "$STAGING/share/glib-2.0/schemas/"
glib-compile-schemas "$STAGING/share/glib-2.0/schemas"
cp -r "$MINGW_SYSROOT/share/icons/Adwaita" "$STAGING/share/icons/"
[[ -d "$MINGW_SYSROOT/share/icons/hicolor" ]] && \
    cp -r "$MINGW_SYSROOT/share/icons/hicolor" "$STAGING/share/icons/"
gtk4-update-icon-cache -q "$STAGING/share/icons/Adwaita" || true

# --- 3. zip -----------------------------------------------------------------
(cd dist && zip -qr "$BUNDLE_NAME.zip" "$BUNDLE_NAME")
echo "packed: dist/$BUNDLE_NAME.zip"
