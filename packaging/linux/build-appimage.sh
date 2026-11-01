#!/usr/bin/env bash
# Builds Sound-Pro-<version>-x86_64.AppImage from a release binary (SPEC-024).
#
# Audio is intentionally NOT bundled: the AppImage uses the host's ALSA and
# its distro-configured PipeWire/PulseAudio plugin. Fedora (and every modern
# desktop distro) ships alsa-lib and pipewire-alsa out of the box, so sound
# works with nothing to install. Bundling a distro-built libasound.so.2 plus
# ALSA plugins is the opposite of portable: the bundled libasound tries to
# dlopen its own plugin from inside the AppDir, and a plugin built for another
# distro's ABI fails to load ("Cannot open shared library
# libasound_module_pcm_pipewire.so"). Relying on the host's ALSA/PipeWire is
# the reliable path.
# Usage: build-appimage.sh <sound-pro binary> <version> <output dir>
set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <sound-pro binary> <version> <output dir>" >&2
    exit 2
fi
binary=$1
version=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

appdir=$work/SoundPro.AppDir
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
    "$appdir/usr/share/icons/hicolor/scalable/apps" \
    "$appdir/usr/share/icons/hicolor/256x256/apps"
install -m 755 "$binary" "$appdir/usr/bin/sound-pro"
install -m 644 "$here/sound-pro.desktop" "$appdir/sound-pro.desktop"
install -m 644 "$here/sound-pro.desktop" "$appdir/usr/share/applications/sound-pro.desktop"
install -m 644 "$here/../sound-pro.svg" "$appdir/sound-pro.svg"
install -m 644 "$here/../sound-pro.svg" \
    "$appdir/usr/share/icons/hicolor/scalable/apps/sound-pro.svg"
install -m 644 "$here/../sound-pro.png" "$appdir/sound-pro.png"
install -m 644 "$here/../sound-pro.png" \
    "$appdir/usr/share/icons/hicolor/256x256/apps/sound-pro.png"
# Newer appimagetool builds require a raster icon and .DirIcon.
ln -s sound-pro.png "$appdir/.DirIcon"

# NOTE: we deliberately do NOT bundle the audio or GUI/display closures
# (libasound, ALSA plugins, X11, GL, EGL, Xcursor, wayland, vendor GPU
# drivers, ...). Those are provided by any desktop host. In particular the
# host ALSA + its distro-configured PipeWire/PulseAudio plugin must stay
# host-side: bundling a libasound.so.2 and plugin built for another distro's
# ABI fails to dlopen on the target ("Cannot open shared library
# libasound_module_pcm_pipewire.so") and silently kills audio. Fedora ships
# alsa-lib and pipewire-alsa by default, so this works out of the box.

# --- AppRun -----------------------------------------------------------------
# Do not set LD_LIBRARY_PATH or ALSA_PLUGIN_DIR: the host dynamic loader and
# the host ALSA plugin search path must be used so the distro's own
# libasound/PipeWire plugin is found.
cat > "$appdir/AppRun" <<'RUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/sound-pro" "$@"
RUN
chmod 755 "$appdir/AppRun"

tool=${APPIMAGETOOL:-}
if [ -z "$tool" ]; then
    tool=$work/appimagetool
    curl -fsSL -o "$tool" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod 755 "$tool"
fi

mkdir -p "$out"
target=$out/Sound-Pro-$version-x86_64.AppImage
# --appimage-extract-and-run works without FUSE (CI containers).
ARCH=x86_64 "$tool" --appimage-extract-and-run --no-appstream "$appdir" "$target"
echo "$target"
