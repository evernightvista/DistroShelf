#!/usr/bin/env bash
# flatpak-dev-shell.sh — enter an interactive Flatpak SDK dev shell.
#
# Drops you into an interactive bash in the same directory you called the
# script from, inside the prepared flatpak-builder build sandbox (sources
# extracted, meson configured), instead of the meson builddir (_flatpak_build)
# where --build-shell lands by default. Run from a terminal.
# See docs/flatpak-dev-shell.md for the rationale.
set -euo pipefail

MODULE="distroshelf"
MANIFEST="com.ranfdev.DistroShelf.json"
BUILD_DIR="_build"
STATE_DIR=".flatpak-builder"

# Save the caller's working directory so we can enter the same path inside
# the sandbox (where it's accessible via --filesystem=home), instead of the
# ephemeral /run/build/$MODULE copy that flatpak-builder creates.
ORIG_PWD="$PWD"

# Always run from the project root (this script's directory).
cd "$(dirname "$(readlink -f "$0")")"

if [[ ! -d "$BUILD_DIR" ]]; then
    echo "error: '$BUILD_DIR' not found. Build dependencies first:" >&2
    echo "  flatpak-builder --disable-rofiles-fuse --stop-at=$MODULE --force-clean $BUILD_DIR $MANIFEST" >&2
    exit 1
fi

# --build-shell re-extracts the source into a fresh <module>-N directory on
# every run, leaving a trail of stale dirs. Keep only the one the stable
# "<module>" symlink currently points at; the rest are safe to delete.
if [[ -L "$STATE_DIR/build/$MODULE" ]]; then
    keep="$(readlink "$STATE_DIR/build/$MODULE")"
    find "$STATE_DIR/build" -maxdepth 1 -type d -name "$MODULE-*" \
        ! -name "$keep" -exec rm -rf {} +
fi

# --build-shell always launches /bin/sh in the builddir (_flatpak_build) and
# ignores $SHELL. Feed it a cd to ORIG_PWD, then hand control to an interactive
# bash on the controlling terminal (/dev/tty).
exec flatpak-builder --disable-rofiles-fuse --build-shell="$MODULE" \
    "$BUILD_DIR" "$MANIFEST" \
    < <(printf 'cd %s\nexec bash -i </dev/tty\n' "$ORIG_PWD")
