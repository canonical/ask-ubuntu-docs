#!/bin/sh
# Launcher wrapper for ubuntu-desktop-help snap.
#
# LanceDB requires write access to the index directory even for read-only
# queries (lock files, metadata). $SNAP is a read-only squashfs mount, so
# we copy the baked-in index to $SNAP_COMMON on every launch before starting
# the app. At ~30 MB this takes well under a second.
#
# A user-supplied index at $SNAP_USER_DATA/index.lance takes priority
# (checked by index_path() in the binary itself); this wrapper only sets
# up the $SNAP_COMMON fallback.

set -e

DEST="$SNAP_COMMON/index.lance"

# Always refresh the writable copy from the read-only snap image so that
# snap updates automatically bring in the latest index.
rm -rf "$DEST"
cp -r "$SNAP/index.lance" "$DEST"

export UBUNTU_HELP_INDEX_PATH="$DEST"
exec "$SNAP/usr/bin/ubuntu-desktop-help" "$@"
