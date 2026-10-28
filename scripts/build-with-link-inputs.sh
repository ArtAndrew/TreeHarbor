#!/bin/sh
# Build helpers only. Rust remains the application/runtime implementation.
set -eu
if [ "$#" -ne 3 ]; then
    echo 'usage: build-with-link-inputs.sh TARGET NEW_ABSOLUTE_BUILD_ROOT BUILD_SESSION_UUID' >&2
    exit 2
fi
th_target=$1
th_build_root=$2
th_session=$3
th_source_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
case "$th_target" in
    x86_64-unknown-linux-musl) th_arch=x86_64 ;;
    aarch64-unknown-linux-musl) th_arch=aarch64 ;;
    *) echo 'unsupported canonical build target' >&2; exit 2 ;;
esac
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = "$th_arch" ] || {
    echo 'canonical build requires its native Linux architecture' >&2; exit 2;
}
cd "$th_source_root"
[ -z "$(git status --porcelain --untracked-files=normal)" ] || {
    echo 'canonical build requires a clean committed checkout' >&2; exit 2;
}
python3 - "$th_build_root" "$th_source_root" "$th_session" <<'PYTHON'
from pathlib import Path
import re, sys, uuid
build, source = Path(sys.argv[1]), Path(sys.argv[2]).resolve()
session = uuid.UUID(sys.argv[3])
if (not build.is_absolute() or build.is_symlink() or build.exists()
        or not re.fullmatch(r"/[A-Za-z0-9_./+=-]+", str(build))
        or build.resolve().is_relative_to(source)
        or str(session) != sys.argv[3] or session.int == 0):
    raise SystemExit('new external build root and canonical nonzero session required')
build.mkdir(mode=0o700)
for name in ('target', 'captures'):
    (build / name).mkdir(mode=0o700)
(build / 'target/tmp').mkdir(mode=0o700)
PYTHON
export CARGO_TARGET_DIR="$th_build_root/target"
export TMPDIR="$CARGO_TARGET_DIR/tmp"
export TH_LINK_CAPTURE_DIR="$th_build_root/captures"
export TH_LINK_REAL_CC=/usr/bin/cc
export TH_LINK_TARGET="$th_target"
export TH_LINK_SYSROOT_ROOT="$(rustc --print sysroot)"
export TH_LINK_REPO_ROOT="$th_source_root"
export TH_LINK_TARGET_ROOT="$CARGO_TARGET_DIR"
export TH_LINK_CARGO_ROOT="${CARGO_HOME:-$HOME/.cargo}"
export TH_LINK_BUILD_SOURCE="$(git rev-parse HEAD)"
export TH_LINK_BUILD_SESSION="$th_session"
case "$th_arch" in
    x86_64) export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$th_source_root/scripts/capture-link-inputs.py" ;;
    aarch64) export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$th_source_root/scripts/capture-link-inputs.py" ;;
esac
cargo auditable build --locked --release --target "$th_target" -p treeharbor-service -p treeharbor-runtime --bins
python3 scripts/capture-native-inputs.py --capture-dir "$TH_LINK_CAPTURE_DIR" \
    --bin-dir "$CARGO_TARGET_DIR/$th_target/release" --target "$th_target" \
    --source-revision "$TH_LINK_BUILD_SOURCE" --build-session "$th_session" \
    --output-dir "$th_build_root/native-reports"
