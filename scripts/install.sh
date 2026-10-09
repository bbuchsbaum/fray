#!/bin/sh
# Install a built fray binary without overwriting the installed one in place.
#
# Usage: scripts/install.sh [SOURCE] [DEST]
#   SOURCE  the new binary (default: this checkout's target/release/fray)
#   DEST    where it is installed (default $HOME/.cargo/bin/fray)
#
# Copying over an installed executable rewrites it in place. On macOS the
# kernel then kills every later launch of that path (exit 137, no output),
# even though the bytes on disk are correct. This script writes a new file
# beside the target and renames it into place, so the path points at a fresh
# file. It keeps the previous binary as DEST.previous for rollback, and checks
# that both the new file and the installed path actually run.
#
# Installing never restarts a running daemon. Afterwards the new binary lists
# the daemons left on another build (`fray daemons --stale --scan`) and this
# prints the `fray restart --all-stale` commands that would move them onto it.
# That report is advisory: if it fails, the install still succeeds.
set -eu

# CDPATH is cleared so cd prints nothing into the path.
src=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)/target/release/fray}
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
    # Copied, not moved, so DEST never goes missing, even briefly.
    rm -f "$dest.previous"
    cp -p "$dest" "$dest.previous"
fi
mv -f "$tmp" "$dest"
trap - EXIT

if installed=$("$dest" --version); then
    echo "install: $installed at $dest"
    [ -e "$dest.previous" ] && echo "install: previous binary kept at $dest.previous"
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

# Report the daemons still on another build, and how to restart them. Read
# only: this never restarts anything, and a failure here is only a warning.
# --scan finds daemons started before the registry existed (transitional).
if ! stale=$("$dest" daemons --stale --scan 2>&1); then
    echo "install: warning: could not list stale daemons ($dest daemons --stale --scan):" >&2
    printf '%s\n' "$stale" | sed 's/^/install:   /' >&2
    exit 0
fi
# A daemon's state line: two spaces, its state, then its pid and build.
# Only `running` daemons are restarted by --all-stale; unreachable and
# incompatible ones are listed but would be skipped.
listed='^  (running|incompatible|unreachable)( \(unregistered\))?  pid .* STALE'
running='^  running( \(unregistered\))?  pid .* STALE'
if ! printf '%s\n' "$stale" | grep -Eq "$listed"; then
    echo "install: no running daemons are out of date"
    exit 0
fi
echo "install: daemons still running another build:"
printf '%s\n' "$stale" | sed 's/^/install:   /'
if ! printf '%s\n' "$stale" | grep -Eq "$running"; then
    echo "install: none of them answers normally (unreachable or incompatible), so fray restart --all-stale would skip them; see each one above"
    exit 0
fi
# The commands must run this binary: plain `fray` only when it resolves here.
fray=$dest
if [ "$(command -v fray 2>/dev/null || true)" = "$dest" ]; then
    fray=fray
fi
case $fray in
*[!A-Za-z0-9_./+-]*) fray="'$(printf '%s' "$fray" | sed "s/'/'\\\\''/g")'" ;;
esac
scan=
if printf '%s\n' "$stale" | grep -Eq '^  running \(unregistered\)  pid .* STALE'; then
    scan=" --scan"
fi
echo "install: nothing was restarted. To see what a restart would interrupt, then restart them:"
echo "install:   $fray restart --all-stale$scan --dry-run"
echo "install:   $fray restart --all-stale$scan"
