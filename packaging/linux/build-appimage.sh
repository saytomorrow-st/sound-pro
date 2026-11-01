#!/usr/bin/env bash
# Builds Sound-Pro-<version>-x86_64.AppImage from a release binary (SPEC-024).
#
# The image is self-contained for audio: it bundles libasound.so.2 and the
# ALSA plugins that route to PipeWire/PulseAudio, plus their non-glibc
# dependencies, so the end user does not have to install alsa-lib or plugins.
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
libdir=$appdir/usr/lib
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
    "$appdir/usr/share/icons/hicolor/scalable/apps" \
    "$appdir/usr/share/icons/hicolor/256x256/apps" \
    "$libdir/alsa-lib"
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

# --- Self-contained audio bundling -----------------------------------------
# libasound.so.2 and the ALSA plugins live inside the AppDir; AppRun points
# ALSA_PLUGIN_DIRS and LD_LIBRARY_PATH at them so nothing has to be installed
# on the host. We bundle every library that is not part of the base glibc
# runtime (see is_base_lib), so the image stays portable across distributions.
bundle_lib() {
    local src=$1
    local real
    real=$(readlink -f "$src" 2>/dev/null || echo "$src")
    local name
    name=$(basename "$real")
    if [ -e "$libdir/$name" ]; then
        return 0
    fi
    install -m 755 "$real" "$libdir/$name"
    # Re-create the soname symlink (e.g. libasound.so.2 -> libasound.so.2.0.0)
    # so the runtime linker resolves it inside the AppDir.
    local soname
    soname=$(basename "$src")
    if [ "$soname" != "$name" ]; then
        ln -sf "$name" "$libdir/$soname"
    fi
    echo "bundled $soname -> $name" >&2
}

is_base_lib() {
    # Only the base glibc runtime is assumed present on every host; everything
    # else (libasound, ALSA plugins, libstdc++, libgcc_s, libpipewire, ...) is
    # bundled so the AppImage is self-contained.
    case "$1" in
        libc.so*|libm.so*|libdl.so*|libpthread.so*|librt.so*|libresolv.so*|ld-linux*) return 0 ;;
        *) return 1 ;;
    esac
}

# Recursively resolve non-glibc dependencies of an ELF object via ldd.
# Uses a here-string so the loop runs in the current shell (set -e applies).
bundle_deps() {
    local obj=$1
    local line path name
    while IFS= read -r line; do
        case "$line" in
            *"=> not found"*)
                echo "build-appimage: missing dependency for $obj: $line" >&2
                return 1
                ;;
            *"=> "*)
                # line is like: libfoo.so.2 => /lib/x86_64-linux-gnu/libfoo.so.2 (0x...)
                path=$(printf '%s\n' "$line" | sed -n 's/.*=> \([^ ]*\) .*/\1/p')
                name=$(printf '%s\n' "$line" | sed -n 's/^[[:space:]]*\([^ ]*\) .*/\1/p')
                [ -n "$path" ] || continue
                # Ignore vdso / linux-vdso lines.
                case "$path" in
                    *vdso*) continue ;;
                esac
                if is_base_lib "$name"; then
                    continue
                fi
                if bundle_lib "$path"; then
                    bundle_deps "$path"
                fi
                ;;
        esac
    done <<< "$(ldd "$obj" 2>/dev/null || true)"
}

# Bundle ALSA runtime + plugins that make sound work over PipeWire/PulseAudio.
bundle_alsa() {
    local alsa
    alsa=$(ldconfig -p 2>/dev/null | awk '/libasound\.so\.2 /{print $NF; exit}')
    if [ -n "$alsa" ] && [ -e "$alsa" ]; then
        bundle_lib "$alsa"
        bundle_deps "$alsa"
        alsa_bundled=1
    else
        echo "build-appimage: warning: libasound.so.2 not found; AppImage will run without sound" >&2
    fi

    # ALSA plugins: PipeWire first (modern default), then PulseAudio.
    local plugdir
    for candidate in /usr/lib/x86_64-linux-gnu/alsa-lib /usr/lib64/alsa-lib /usr/lib/alsa-lib; do
        [ -d "$candidate" ] && { plugdir=$candidate; break; }
    done
    if [ -n "${plugdir:-}" ]; then
        for plugin in \
            libasound_module_pcm_pipewire.so \
            libasound_module_ctl_pipewire.so \
            libasound_module_pcm_pulse.so \
            libasound_module_ctl_pulse.so; do
            if [ -e "$plugdir/$plugin" ]; then
                install -m 755 "$plugdir/$plugin" "$libdir/alsa-lib/$plugin"
                bundle_deps "$plugdir/$plugin"
            fi
        done
    fi
}

bundle_alsa

# NOTE: we deliberately do NOT bundle the GUI/display closure (X11, GL, EGL,
# Xcursor, wayland, vendor GPU drivers, ...). Those are provided by any desktop
# host and must stay host-side (bundling a vendor libGL breaks the AppImage on
# machines with a different GPU). Only the audio closure — libasound, the ALSA
# plugins, and their non-glibc deps — is made self-contained, which is the
# part that a minimal host (or one without sudo/alsa-lib) cannot be assumed to
# have.

# --- AppRun -----------------------------------------------------------------
# Point the dynamic loader and ALSA at the bundled libraries. We prepend our
# dirs and keep the host path, so libraries we did not bundle still resolve.
cat > "$appdir/AppRun" <<'RUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
lib="$here/usr/lib"
if [ -d "$lib" ]; then
    LD_LIBRARY_PATH="$lib:${LD_LIBRARY_PATH:-}"
    export LD_LIBRARY_PATH
fi
if [ -d "$lib/alsa-lib" ]; then
    ALSA_PLUGIN_DIR="$lib/alsa-lib"
    export ALSA_PLUGIN_DIR
fi
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

# --- Self-contained verification -------------------------------------------
# Fail the build if libasound (when it was available) is not in the AppDir, or
# if any bundled audio library still has an unresolved non-glibc dependency.
# The audio closure must be fully self-contained; the GUI/display closure is
# intentionally host-provided and not checked here.
verify_selfcontained() {
    local missing=0
    if [ -n "${alsa_bundled:-}" ]; then
        if ! find "$libdir" -name 'libasound.so.2*' | grep -q .; then
            echo "build-appimage: ERROR: libasound.so.2 was expected but is not bundled" >&2
            missing=1
        fi
    fi
    # Every bundled audio library must resolve against the bundled libdir with
    # no missing non-glibc dependency.
    local f line name
    while IFS= read -r f; do
        [ -e "$f" ] || continue
        while IFS= read -r line; do
            case "$line" in
                *"=> not found"*)
                    name=$(printf '%s\n' "$line" | sed -n 's/^[[:space:]]*\([^ ]*\) .*/\1/p')
                    if ! is_base_lib "$name"; then
                        echo "build-appimage: ERROR: $(basename "$f") needs unresolved library: $name" >&2
                        missing=1
                    fi
                    ;;
            esac
        done <<< "$(LD_LIBRARY_PATH=$libdir ldd "$f" 2>/dev/null || true)"
    done < <(find "$libdir" -type f | sort -u)
    return "$missing"
}

mkdir -p "$out"
target=$out/Sound-Pro-$version-x86_64.AppImage
# --appimage-extract-and-run works without FUSE (CI containers).
ARCH=x86_64 "$tool" --appimage-extract-and-run --no-appstream "$appdir" "$target"
verify_selfcontained
echo "$target"
