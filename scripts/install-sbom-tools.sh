#!/bin/sh
# Linux build tooling only. No installation into the host's system directories.
set -eu
arch=$1
destination=$2
case "$arch" in
  x86_64)
    cargo_sha=7676443367c5e33d9fb22acef63cccb5e9202de12dae1b0579995eb3e2bc7a18
    syft_arch=amd64
    syft_sha=bb65249542db5fedeeb4d3f9b018b75e7264dbfbbb10486f044581a92f90490d
    ;;
  aarch64)
    cargo_sha=c324af56990601354bbf40080f582ba46f8ec8b807a1388e25bb7e429d285c18
    syft_arch=arm64
    syft_sha=929bd163db2d7cd2255d3bcb8967208079813d7b46b0c952f7b794d8e2c7d304
    ;;
  *) echo 'unsupported build-tool architecture' >&2; exit 1 ;;
esac
test "$(uname -s)" = Linux
test "$(uname -m)" = "$arch"
case "$destination" in /*) ;; *) echo 'absolute tool destination required' >&2; exit 1 ;; esac
# Refuse to replace an existing installation, including symlink destinations.
mkdir "$destination"
cargo_name="cargo-auditable-$arch-unknown-linux-musl"
curl --fail --location --retry 3 --max-time 180 --output "$destination/cargo.tar.xz" \
  "https://github.com/rust-secure-code/cargo-auditable/releases/download/v0.7.7/$cargo_name.tar.xz"
printf '%s  %s\n' "$cargo_sha" "$destination/cargo.tar.xz" | sha256sum --check --strict
tar -xJf "$destination/cargo.tar.xz" -C "$destination" --strip-components=1 \
  "$cargo_name/cargo-auditable" "$cargo_name/LICENSE-MIT" "$cargo_name/LICENSE-APACHE"
curl --fail --location --retry 3 --max-time 180 --output "$destination/syft.tar.gz" \
  "https://github.com/anchore/syft/releases/download/v1.55.0/syft_1.55.0_linux_$syft_arch.tar.gz"
printf '%s  %s\n' "$syft_sha" "$destination/syft.tar.gz" | sha256sum --check --strict
tar -xzf "$destination/syft.tar.gz" -C "$destination" syft LICENSE
test -x "$destination/cargo-auditable"
test -x "$destination/syft"
