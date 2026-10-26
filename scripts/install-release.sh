#!/bin/sh
# Offline installation from an extracted, independently verified release archive.
set -eu
umask 022
if [ "$#" -ne 2 ] || [ "$1" != '--prefix' ]; then
    echo 'Usage: ./install.sh --prefix /absolute/install/prefix' >&2
    exit 2
fi
prefix=$2
case "$prefix" in /*) ;; *) echo 'Prefix must be absolute.' >&2; exit 2;; esac
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
version=$(cat "$root/VERSION")
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    echo 'Invalid package version.' >&2
    exit 1
fi
(cd "$root" && sha256sum --check --strict SHA256SUMS >/dev/null)
mkdir -p "$prefix/lib/treeharbor" "$prefix/bin"
lock="$prefix/lib/treeharbor/.install-lock"
if ! mkdir "$lock" 2>/dev/null; then
    echo 'Another install holds the lock. If it crashed, remove the empty .install-lock directory before retrying.' >&2
    exit 1
fi
staging=''
cleanup() {
    if [ -n "$staging" ]; then rm -rf -- "$staging"; fi
    rmdir -- "$lock"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
destination="$prefix/lib/treeharbor/$version"
command_path="$prefix/bin/treeharbor"
if [ -e "$destination" ] || [ -L "$destination" ] || [ -e "$command_path" ] || [ -L "$command_path" ]; then
    echo 'Destination or command already exists; choose a new prefix. No files replaced.' >&2
    exit 1
fi
staging=$(mktemp -d "$prefix/lib/treeharbor/.install.XXXXXX")
cp -R "$root/." "$staging/"
# Rename a complete package, then expose its CLI. No service/state is modified.
mv -- "$staging" "$destination"
ln -s -- "$destination/bin/treeharbor" "$command_path"
printf 'Installed: %s\nRuntime init: %s\n' "$command_path" "$destination/bin/treeharbor-runtime-init"
printf 'Add %s/bin to PATH. PostgreSQL and Docker/gVisor setup are separate.\n' "$prefix"
