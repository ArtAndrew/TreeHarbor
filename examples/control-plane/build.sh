#!/bin/sh
set -eu
umask 077

package=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
version=$(cat "$package/VERSION")
(cd "$package" && sha256sum --check --strict SHA256SUMS >/dev/null)
metadata=$(python3 - "$package/manifest.json" "$version" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as manifest:
    package = json.load(manifest)
platforms = {"x86_64-unknown-linux-musl": "linux/amd64", "aarch64-unknown-linux-musl": "linux/arm64"}
platform = platforms.get(package.get("target"))
revision = package.get("source_revision", "")
version = sys.argv[2]
if (package.get("product_version") != version or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", version)
        or not platform or not re.fullmatch(r"[a-f0-9]{40}", revision)):
    raise SystemExit("Invalid TreeHarbor package metadata")
print(platform, revision)
PY
)
platform=${metadata%% *}
revision=${metadata#* }
image=${1:-treeharbor-control-plane:$version}
context=$(mktemp -d)
trap 'rm -rf -- "$context"' EXIT

rootfs="$context/rootfs"
install -d -m 0755 "$rootfs/bin" "$rootfs/usr/share/doc/treeharbor"
install -m 0555 "$package/bin/treeharbor" "$rootfs/bin/treeharbor"
install -m 0444 "$package/LICENSE" "$package/NOTICE" "$package/THIRD_PARTY_NOTICES.md" \
    "$rootfs/usr/share/doc/treeharbor/"
tar -C "$rootfs" --owner=0 --group=0 --numeric-owner -cf "$context/rootfs.tar" bin usr
docker image import --platform "$platform" \
    --change 'LABEL org.opencontainers.image.title="TreeHarbor control plane"' \
    --change "LABEL org.opencontainers.image.version=$version" \
    --change "LABEL org.opencontainers.image.revision=$revision" \
    --change 'LABEL org.opencontainers.image.source=https://github.com/ArtAndrew/TreeHarbor' \
    --change 'LABEL org.opencontainers.image.licenses=Elastic-2.0' \
    --change 'USER 65532:65532' \
    --change 'ENTRYPOINT ["/bin/treeharbor"]' \
    "$context/rootfs.tar" "$image"
