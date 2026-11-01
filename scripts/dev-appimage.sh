#!/usr/bin/env bash
# Obtains a dev AppImage of Sound Pro and drops it into the user's Downloads
# folder, so intermediate results of every PR can be tried out.
#
# Default behaviour: download the prebuilt AppImage from GitHub Releases. This
# needs only `gh`/`curl` — no Rust toolchain and no ALSA headers, so it works
# on a machine without sudo and without alsa-lib-devel (e.g. Fedora with
# `FEATURES=none` today).
#
# With `--build`: build the AppImage locally from source. This is the path
# AGENTS.md documents for CI-less development; it needs a Rust toolchain, and
# for sound it needs the ALSA dev headers (libasound2-dev). When the ALSA dev
# headers are missing, the script prints a clear message and, unless
# `FEATURES=none`, refuses to silently build a silent image.
#
# Usage:
#   scripts/dev-appimage.sh             # download latest release AppImage
#   scripts/dev-appimage.sh --build     # build locally (with sound)
#   FEATURES=none scripts/dev-appimage.sh --build  # build silently, no ALSA
#
# Environment:
#   DEV_APPIMAGE_DIR   output folder (default: xdg-user-dir DOWNLOAD,
#                      then ~/Загрузки, then ~/Downloads)
#   DEV_APPIMAGE_REPO  GitHub repo to download from (default: the remote
#                      "origin" of this checkout)
#   APPIMAGETOOL       existing appimagetool binary (skips the download)
#   FEATURES           cargo features for daw-gui (default: audio; `none`
#                      builds without the audio feature)
#
# Output: Sound-Pro-<version>-x86_64.AppImage (downloaded) or
# Sound-Pro-<version>-dev+<sha>-x86_64.AppImage (built) plus the stable alias
# Sound-Pro-dev-x86_64.AppImage.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

mode=${1:-download}
case "$mode" in
    download|--build) ;;
    *)
        echo "usage: $0 [download|--build]" >&2
        exit 2
        ;;
esac

# --- Resolve the output folder --------------------------------------------
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

# --- Download path ----------------------------------------------------------
download_from_release() {
    local repo=${DEV_APPIMAGE_REPO:-}
    if [ -z "$repo" ]; then
        repo=$(git config --get remote.origin.url 2>/dev/null \
            | sed -E 's#(https?://[^/]+/|git@[^:]+:|\.git$)##g' | sed 's#^/##' || true)
    fi
    if [ -z "$repo" ]; then
        echo "dev-appimage: cannot determine the GitHub repo; set DEV_APPIMAGE_REPO" >&2
        exit 1
    fi

    local asset
    if command -v gh >/dev/null 2>&1; then
        asset=$(gh release view --repo "$repo" --json assets --jq \
            '[.assets[].name | select(endswith("x86_64.AppImage"))][0] // empty' 2>/dev/null || true)
    fi
    if [ -z "${asset:-}" ]; then
        echo "dev-appimage: no x86_64 AppImage in the latest release of $repo." >&2
        echo "dev-appimage: fall back to a local build: scripts/dev-appimage.sh --build" >&2
        exit 1
    fi

    local dest=$out/Sound-Pro-dev-x86_64.AppImage
    # Download into a temp dir first (so we never leave a half-written or
    # stale alias), then move to the stable name.
    local tmp
    tmp=$(mktemp -d)
    if command -v gh >/dev/null 2>&1; then
        gh release download --repo "$repo" --pattern "$asset" --dir "$tmp" \
            --clobber >/dev/null
    else
        curl -fL --retry 3 \
            "https://github.com/$repo/releases/latest/download/$asset" \
            -o "$tmp/$asset"
    fi
    local dl
    dl=$(ls -t "$tmp"/*-x86_64.AppImage 2>/dev/null | head -n 1)
    if [ -z "${dl:-}" ]; then
        echo "dev-appimage: download of $asset produced no AppImage file" >&2
        rm -rf "$tmp"
        exit 1
    fi
    mv -f "$dl" "$dest"
    rm -rf "$tmp"
    chmod 755 "$dest"
    echo "dev-appimage: downloaded $asset -> $dest" >&2
    echo "$dest"
    return 0
}

if [ "$mode" = download ]; then
    download_from_release
    exit 0
fi

# --- Local build path ------------------------------------------------------
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
    # alsa-lib-devel package. Installing one needs root and is not our job.
    # On hosts without it we refuse to silently produce a silent AppImage: the
    # user asked for sound, so tell them clearly what is missing and how to
    # get a sound build without installing anything (download the CI artifact).
    if ! { pkg-config --exists alsa 2>/dev/null && ldconfig -p 2>/dev/null | grep -q 'libasound\.so '; }; then
        if ! { pkg-config --exists alsa 2>/dev/null || ldconfig -p 2>/dev/null | grep -q 'libasound\.so\.2 '; }; then
            echo "dev-appimage: ERROR: the 'audio' feature needs ALSA, but the ALSA" >&2
            echo "  development files (libasound2-dev / alsa-lib-devel) are not installed." >&2
            echo "  Installing them needs root, which is not available here." >&2
            echo "  Options:" >&2
            echo "    1. Download the self-contained CI build instead (no install needed):" >&2
            echo "         scripts/dev-appimage.sh" >&2
            echo "    2. Build a silent AppImage without sound:" >&2
            echo "         FEATURES=none scripts/dev-appimage.sh --build" >&2
            exit 1
        fi
        # Runtime libasound is present but the dev headers are not. Point
        # pkg-config at a stub bound to the installed runtime library, so the
        # build can link against it without the dev package.
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
