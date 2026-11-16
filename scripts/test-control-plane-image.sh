#!/bin/bash
set -euo pipefail
umask 077

package=$1
output=$2
version=$(cat "$package/VERSION")
architecture=$(uname -m)
case "$architecture" in
    x86_64) platform=amd64 ;;
    aarch64) platform=arm64 ;;
    *) printf '%s\n' 'Unsupported native Docker runner' >&2; exit 1 ;;
esac
tag="treeharbor-control-plane:$version-$platform"
archive="treeharbor-$version-$architecture-unknown-linux-musl.control-plane.tar.gz"
scratch=$(mktemp -d)
container=
cleanup() {
    if test -n "$container"; then docker rm "$container" >/dev/null; fi
    docker image rm "$tag" >/dev/null 2>&1 || true
    rm -rf -- "$scratch"
}
trap cleanup EXIT

sh "$package/examples/control-plane/build.sh" "$tag"
image_id=$(docker image inspect "$tag" --format '{{.Id}}')
test "$(docker image inspect "$tag" --format '{{.Os}}/{{.Architecture}} {{.Config.User}}')" = "linux/$platform 65532:65532"
docker image save "$tag" | gzip -n > "$scratch/$archive"
docker image rm "$tag" >/dev/null
docker image load --input "$scratch/$archive"
test "$(docker image inspect "$tag" --format '{{.Id}}')" = "$image_id"
container=$(docker create --pull=never --network=none --read-only --cap-drop=ALL \
    --security-opt=no-new-privileges "$tag" --version)
docker cp "$container:/bin/treeharbor" "$scratch/treeharbor"
cmp "$package/bin/treeharbor" "$scratch/treeharbor"
for notice in LICENSE NOTICE THIRD_PARTY_NOTICES.md; do
    docker cp "$container:/usr/share/doc/treeharbor/$notice" "$scratch/$notice"
    cmp "$package/$notice" "$scratch/$notice"
done
test "$(docker start --attach "$container")" = "treeharbor $version"
test "$(docker inspect "$container" --format '{{.State.ExitCode}}')" = 0

(cd "$scratch" && sha256sum "$archive" > "$archive.sha256")
mv "$scratch/$archive" "$scratch/$archive.sha256" "$output/"
printf 'PASS loaded control-plane image on linux/%s and compared its binary and notices\n' "$platform"
