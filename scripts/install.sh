#!/bin/sh
# Install a built fray binary without overwriting the installed one in place.
#
# Usage: scripts/install.sh [SOURCE] [DEST]
#   SOURCE  the new binary (default target/release/fray)
#   DEST    where it is installed (default $HOME/.cargo/bin/fray)
#
# Copying over an installed executable rewrites it in place. On macOS the
# kernel then kills every later launch of that path (exit 137, no output),
# even though the bytes on disk are correct. This script writes a new file
# beside the target and renames it into place, so the path points at a fresh
# file. It keeps the previous binary as DEST.previous for rollback, and checks
# that both the new file and the installed path actually run.
#
# Installing does not restart a running daemon: announce the restart on the
# board, check that no one has a live wait, then `fray stop` and `fray start`.
set -eu

src=${1:-target/release/fray}
dest=${2:-$HOME/.cargo/bin/fray}

if [ ! -x "$src" ]; then
    echo "install: no executable at $src (build with: cargo build --locked --release)" >&2
    exit 1
fi
if ! new_version=$("$src" --version); then
    echo "install: $src does not run; nothing was installed" >&2
    exit 1
fi

tmp="$dest.new.$$"
trap 'rm -f "$tmp"' EXIT
cp "$src" "$tmp"
chmod 755 "$tmp"
if ! "$tmp" --version >/dev/null; then
    echo "install: the copy at $tmp does not run; nothing was installed" >&2
    exit 1
fi
if [ -e "$dest" ]; then
    # A copy into a new file, never a move, so running daemons keep theirs.
    rm -f "$dest.previous"
    cp -p "$dest" "$dest.previous"
fi
mv -f "$tmp" "$dest"
trap - EXIT

if installed=$("$dest" --version); then
    echo "install: $installed at $dest"
    [ -e "$dest.previous" ] && echo "install: previous binary kept at $dest.previous"
    echo "install: restart the daemon when it is safe: announce it, then fray stop && fray start"
else
    status=$?
    echo "install: $dest does not run (exit $status)." >&2
    if [ "$status" -eq 137 ]; then
        echo "install: exit 137 is SIGKILL; on macOS that follows an in-place overwrite. Run this script again; do not cp over the binary." >&2
    fi
    exit 1
fi
[ "$installed" = "$new_version" ] || {
    echo "install: installed version '$installed' differs from '$new_version'" >&2
    exit 1
}
