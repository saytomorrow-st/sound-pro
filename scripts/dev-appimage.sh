#!/usr/bin/env bash
# Builds a local dev AppImage of Sound Pro and drops it into the user's
# Downloads folder, so intermediate results of every PR can be tried out.
#
# Usage: scripts/dev-appimage.sh
#
# Environment:
#   DEV_APPIMAGE_DIR   output folder (default: xdg-user-dir DOWNLOAD,
#                      then ~/Загрузки, then ~/Downloads)
#   APPIMAGETOOL       existing appimagetool binary (skips the download)
#   FEATURES           cargo features for daw-gui (default: audio; `none`
#                      builds without the audio feature)
#
# Output: Sound-Pro-<version>-dev+<sha>-x86_64.AppImage plus the stable
# alias Sound-Pro-dev-x86_64.AppImage; only the 3 newest builds of this base
# version are kept.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if [ -z "${DEV_APPIMAGE_DIR:-}" ]; then
    if command -v xdg-user-dir >/dev/null 2>&1; then
        DEV_APPIMAGE_DIR=$(xdg-user-dir DOWNLOAD || true)
    fi
fi
if [ -z "${DEV_APPIMAGE_DIR:-}" ] || [ "$DEV_APPIMAGE_DIR" = "$HOME" ]; then
    for candidate in "$HOME/Загрузки" "$HOME/Downloads"; do
        if [ -d "$candidate" ]; then
            DEV_APPIMAGE_DIR=$candidate
            break
        fi
    done
fi
mkdir -p "$DEV_APPIMAGE_DIR"
out=$(cd "$DEV_APPIMAGE_DIR" && pwd)

version=$(sed -n -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    echo "dev-appimage: cannot read version from Cargo.toml" >&2
    exit 1
fi
sha=$(git rev-parse --short=7 HEAD)
branch=$(git rev-parse --abbrev-ref HEAD)
version=$version-dev+$sha
# The alias is linked, so there is no second copy of the image; copy only
# when the folder lives on another filesystem.
stable=Sound-Pro-dev-x86_64.AppImage
# The audio feature is what release builds ship, so it is the default here
# too; FEATURES=none builds a silent GUI (used on hosts without ALSA dev
# files).
features=${FEATURES:-audio}
if [ "$features" = none ]; then
    features=
    feature_args=
else
    feature_args="--features $features"
fi

echo "dev-appimage: $branch at $sha (features: ${features:-none})" >&2
case " $features " in
*" audio "*)
    # CPAL probes ALSA through pkg-config, which needs alsa.pc from the
    # alsa-lib-devel package. Installing one needs root and is not our job;
    # when it is missing, point pkg-config at a stub bound to the installed
    # runtime library. The stub lives in target/; nothing outside the repo
    # and the output folder is touched. A build without the dev package may
    # still fail to link when the linker cannot resolve libasound.so.
    if ! { pkg-config --exists alsa 2>/dev/null && ldconfig -p 2>/dev/null | grep -q 'libasound\.so '; }; then
        stub=$root/target/dev-alsa-pkgconfig
        mkdir -p "$stub"
        cat >"$stub/alsa.pc" <<'PC'
prefix=/usr
libdir=/usr/lib64

Name: alsa
Description: ALSA runtime library (build-time probe only)
Version: 1.2.14
Libs: -L${libdir} -lasound
Cflags:
PC
        # An absolute path: cargo runs build scripts from each crate's dir.
        export PKG_CONFIG_PATH=$stub${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}
        echo "dev-appimage: alsa-lib-dev not found, using $stub/alsa.pc" >&2
    fi
    ;;
esac
# shellcheck disable=SC2086 # feature_args is a plain "--features x" pair.
cargo build --release -p daw-gui $feature_args

built=Sound-Pro-$version-x86_64.AppImage
packaging/linux/build-appimage.sh target/release/sound-pro "$version" "$out" >/dev/null
chmod 755 "$out/$built"
ln -f "$out/$built" "$out/$stable" 2>/dev/null || cp -f "$out/$built" "$out/$stable"
chmod 755 "$out/$stable"

# Keep the 3 newest dev builds of this base version. The list is collected
# first, so the loop runs in this shell where `set -e` still applies.
old_builds=$(printf '%s\n' "$out"/Sound-Pro-*-dev+*-x86_64.AppImage | sort -r | tail -n +4)
while IFS= read -r old; do
    [ -e "$old" ] || continue
    rm -f -- "$old"
    echo "dev-appimage: removed older build $(basename "$old")" >&2
done <<EOF
$old_builds
EOF

echo "dev-appimage: stable alias $out/$stable -> $(basename "$built")" >&2
echo "$out/$built"
