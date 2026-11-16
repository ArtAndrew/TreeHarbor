# Run the TreeHarbor control plane in Docker

Use a Linux Docker host and a local PostgreSQL 18 instance. The image contains
the static server and administration CLI. Run agent workers and GitHub bridges
on the host or on their own nodes. The service uses local objects and a
PostgreSQL Unix socket.

## Install a release image

Start with the [signed package installation](../../docs/try-treeharbor.md#1-install-a-signed-linux-package)
and continue in the same shell. Use `v0.1.23-alpha.1` or a release that includes
the `.control-plane.tar.gz` asset. The package provides the host CLI and the
runtime helpers. The image runs on the same Linux architecture.

```sh
cd "$TH_TRIAL"
TH_IMAGE_ARCHIVE="$TH_ASSET.control-plane.tar.gz"
gh release download "$TH_TAG" --repo "$TH_REPO" \
  --pattern "$TH_IMAGE_ARCHIVE" --pattern "$TH_IMAGE_ARCHIVE.sha256"
gh attestation verify "$TH_IMAGE_ARCHIVE" \
  --bundle "$TH_ASSET.provenance.sigstore.json" --repo "$TH_REPO" \
  --signer-workflow "$TH_REPO/.github/workflows/release-provenance.yml" \
  --signer-digest "$TH_SOURCE" --source-digest "$TH_SOURCE" \
  --source-ref "refs/tags/$TH_TAG" --deny-self-hosted-runners
sha256sum --check --strict "$TH_IMAGE_ARCHIVE.sha256"
case "$TH_ARCH" in
  x86_64) TH_PLATFORM=amd64 ;;
  aarch64) TH_PLATFORM=arm64 ;;
esac
TH_IMAGE="treeharbor-control-plane:$TH_VERSION-$TH_PLATFORM"
docker image load --input "$TH_IMAGE_ARCHIVE"
docker run --rm --network=none "$TH_IMAGE" --version
```

## Build from an extracted package

The build script uses Docker's local image import and selects the platform from
the package manifest. It needs Python 3.11+ and can prepare either Linux
architecture from the corresponding verified package. [Docker image import](https://docs.docker.com/reference/cli/docker/image/import/)

From the extracted package directory, build the image and check its version.

```sh
TH_IMAGE="treeharbor-control-plane:$(cat VERSION)"
sh examples/control-plane/build.sh "$TH_IMAGE"
docker run --rm --network=none "$TH_IMAGE" --version
```

To transfer the image to another Docker host, save and load the resulting tar
archive. The image records the signed package's source revision in its labels.
[Docker image save](https://docs.docker.com/reference/cli/docker/image/save/)

```sh
docker image save --output treeharbor-control-plane.tar "$TH_IMAGE"
docker image load --input treeharbor-control-plane.tar
```

## Run the service

Create a persistent directory owned by the non-root operator. Set
`TH_PG_SOCKET_DIR` to the absolute directory containing the PostgreSQL socket.
Adjust the database and role in the private config below. The role needs to
own the configured schema. Use an operator-controlled directory for the socket
and the [usual database authentication](../../docs/local-service.md).

```sh
umask 077
TH_STATE=$(mktemp -d /tmp/treeharbor-container.XXXXXX)
mkdir -m 700 "$TH_STATE/objects"
cat > "$TH_STATE/config.json" <<'JSON'
{
  "database_url": "host=/pg user=treeharbor dbname=treeharbor",
  "socket": "/state/service.sock",
  "storage": {"root": "/state/objects", "backend": "local"}
}
JSON

th_container() {
  docker run --rm --network=none --read-only --cap-drop=ALL \
    --security-opt=no-new-privileges --user "$(id -u):$(id -g)" \
    --mount "type=bind,source=$TH_STATE,target=/state" \
    --mount "type=bind,source=$TH_PG_SOCKET_DIR,target=/pg,readonly" \
    "$TH_IMAGE" "$@"
}

th_container admin --config /state/config.json migrate
th_container doctor --config /state/config.json
th_container serve --config /state/config.json
```

Connect the installed host CLI to `$TH_STATE/service.sock` using the scoped
credential file for the repository. Run administrator commands through
`th_container admin --config /state/config.json`. For example, create a
repository with `--authority connected` for the [GitHub flow](../../docs/connected-pr-preview.md)
or `--authority standalone` for the [native flow](../../docs/native-integration.md).
Write credential output to `/state` and use the corresponding host path.
Use a persistent path in place of `/tmp` for state you intend to keep.

The service uses the two bind mounts shown above and runs with networking
disabled. Agent execution uses the separate
[runtime configuration](../../docs/managed-runs.md). Stop the
service with Ctrl-C or Docker's stop command. Use host-side PostgreSQL tooling
and the mounted object directory for a [cold backup](../../docs/backup-restore.md).
